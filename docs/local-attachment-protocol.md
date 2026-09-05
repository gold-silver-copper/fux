# Local attachment protocol v2

A workspace attachment socket resides in the private fux runtime directory. The server owns its socket path and only removes the inode it bound. Both client and server authenticate the kernel peer UID against their effective UID. Fux does not authenticate remote users; an optional koh gateway must do so before opening this interface.

Each frame is a four-byte big-endian unsigned length followed by UTF-8 JSON. Empty frames are invalid. Client frames are capped at 64 KiB, with input chunks capped at 4096 bytes. Server frames are capped at 16 MiB. A frame must finish within five seconds after its first byte; writes also have a five-second deadline. Initial handshake has a five-second deadline including idle time. Partial-frame readers may only be cancelled when discarding the connection.

Client messages use an internal `type` tag:

- `{"type":"hello","version":2,"rows":24,"columns":80}`
- `{"type":"pane-input","bytes":[27,91,65]}`
- `{"type":"mouse","event":{"code":0,"column":2,"row":2,"release":false}}`
- `{"type":"binding","key":101}`
- `{"type":"input","bytes":[27,91,65]}` (raw-input alias for embedded consumers)
- `{"type":"control","request":CONTROL_REQUEST}`
- `{"type":"copy-view","request":1,"pane":1,"offset":3}`
- `{"type":"resize","rows":40,"columns":120}`
- `{"type":"detach"}`

Server messages use an external variant tag so typed numeric workspace map keys deserialize directly through JSON's map-key reader:

- `{"hello":{"version":2}}`
- `{"state":{"state":WORKSPACE_STATE}}`
- `{"reply":{"reply":CONTROL_REPLY}}`
- `{"copy-view":{"reply":{"request":1,"pane":1,"view":PANE_VIEW}}}`
- `{"error":{"message":"..."}}`
- `{"exited":{"code":0}}` (code may be null)

The client negotiates before entering terminal raw mode. The server sends an initial full state and coalesces subsequent state changes without accumulating repaint queues. At most 64 clients can occupy a workspace endpoint, including handshakes. Slow writers are disconnected. Each viewer receives a unique local ID and detach releases its viewport without terminating pane processes.

Viewer control requests use the existing typed control schema and execute in order with input.
After executing a request, the server sends its resulting state before its reply. The viewer keeps
one request outstanding and reads that state before interpreting the next command, including when
several commands arrive in one terminal read. Replies and interaction panels belong to the requesting
viewer. These messages require attachment version 2. A version 1 peer is rejected before the viewer enters
terminal raw mode. Save work and explicitly restart an old server when migrating; fux never kills
an existing session to upgrade it.

Copy-view reads return one bounded pane viewport privately to the requesting connection. Request
IDs are echoed so a viewer can discard results from an obsolete interaction. The offset is clamped
to retained history; a missing pane produces `view: null`. Received pane geometry and cell data are
validated before delivery to the viewer. Reading history does not change shared selection, clipboard
or viewport state. The viewer connection offers a bounded reply channel; the copy controller must
keep its requests bounded and consume their replies.

The current standalone binary uses this interface for local attach and has no koh or zor crate dependency. The optional koh gateway preserves this opaque attachment stream across reconnects. Workspace and manager commands use the separate [control protocol](local-control-protocol.md). Verification evidence and platform limits are recorded in [the completion audit](standalone-audit.md).

All attachment terminal bytes bypass the shared legacy prefix parser: the production viewer uses
`pane-input`, and `input` is an equivalent raw-input alias. Prefix commands must be interpreted by
the viewer and sent through explicit control/binding messages.
A literal prefix is sent once, unchanged. Application mouse events and external binding keys are
explicit messages, ordered with terminal input. The host resolves an external key from its own
configuration and acknowledges its dispatch with `accepted`; completion of the external process is
asynchronous. The viewer never transmits executable argv for a binding.
Mouse forwarding cannot enable the host's legacy shared copy/scrollback behavior.
