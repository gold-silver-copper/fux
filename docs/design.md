# Standalone architecture

Fux owns local terminal multiplexing. Koh and zor are independent optional applications connected
through versioned local interfaces. Neither appears in fux's default dependency graph, and local
startup does not create identities or open network listeners.

## Ownership

| Project | Responsibility | Boundary |
|---|---|---|
| fux | Pane PTYs/process groups, terminal emulation, workspaces/tabs/layouts, rendering/input, scrollback, commands and local configuration | Local Unix attachment and control protocols |
| koh | Remote transports, identities, encryption, authorization, discovery, relays and reconnect | Authenticated gateway to one fixed local service |
| zor | Agent detection, observation rules/state machine and event schema | Optional sidecar sampling fux control and emitting bounded metadata |

Fux incorporates only the terminal primitives needed by a local multiplexer and a consumer adapter
for the observation schema, with original licenses retained. It contains neither koh's network
stack nor zor's detection logic.

## Persistent local sessions

One session server per runtime directory/user starts on demand. It owns named workspaces and their
pane processes. Each workspace has a private attachment socket and control socket; the manager
resolves names and handles lifecycle requests. Descriptors contain PID, instance nonce, socket path,
and protocol version. Startup locks serialize first launches, and inode-aware cleanup preserves
replacement filesystem nodes.

Viewers connect through Unix sockets after ownership, permission, and kernel peer checks. Version
negotiation completes before the client creates its raw terminal backend or input workers. Viewer
detach drops its viewport, not the workspace. An explicit workspace kill or server shutdown ends
its panes. A version mismatch reports a restart requirement and never silently terminates sessions.

The [control protocol](local-control-protocol.md) negotiates its version before any command or subscription.
The [attachment protocol](local-attachment-protocol.md) carries bounded input, resize, detach,
workspace snapshots, errors, and exit status. Slow connections have bounded frame/write deadlines;
there are no unbounded output queues. Terminal state remains authoritative in the local host.

## Optional remote gateway

Koh authorizes TLS peers before opening the configured local fux socket. On the viewer machine,
koh exposes a private proxy socket to `fux attach --socket PATH`. Fux sees the same local protocol
and knows nothing about remote identities or transports.

Koh owns acknowledged, bounded forwarding and retry journals. Sessions are scoped by authenticated
peer and random token. A transient link loss can resume the same Unix stream; a missing or expired
resume cannot create a new application connection. Retry/retention lasts up to 30 seconds after
loss is detected. Gateway restart requires a fresh attachment and leaves fux pane processes alive.

## Optional observation

Pane commands always start directly. With `zor-path` configured, fux starts a separately supervised
`zor observe` process after the pane's control interface is available. Zor samples bounded capture,
geometry, title, progress and process information and runs its own rules/state machine. Fux applies
reports only as bounded presentation metadata.

Observer crashes, invalid reports, missing binaries and stalls do not terminate panes. Fux clears
stale status when the observer exits and owns/reaps that observer process group. Pane output may
also include agent reports; neither path makes status an authenticated claim.

## Verification and history

Default CI builds/tests/packages a fux-only checkout. Optional manual integration CI reconstructs
owner repositories from pinned sources. Native runtime evidence, cross-checks and remaining audit
work are recorded in [standalone-plan.md](standalone-plan.md). The [security model](security.md)
details local trust boundaries.

The [earlier design](design-before-standalone.md) is retained as historical context. Its embedded
networking, key commands and per-pane wrapper architecture do not describe current fux.
