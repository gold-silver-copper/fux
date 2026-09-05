# Local control protocol v1

Fux exposes a workspace control socket at `RUNTIME/fux/NAME.sock` and a manager socket at
`RUNTIME/fux/manager.sock`. These are separate from the length-prefixed attachment socket.
The OS user is the authorization boundary. The server checks peer credentials before accepting
a connection; fux control clients check the server's peer credentials too.

## Version negotiation

Every connection begins with an eight-byte client preface, exactly `FUXCTL1\n`. The server reads
that preface and replies with its supported eight-byte preface, currently `FUXCTL1\n`. Both peers
must verify equality before sending or dispatching commands. A missing or mismatched preface is
rejected; no command from that connection is executed. Incompatible clients should report the
mismatch and ask the operator to use matching versions or save work before restarting the server.

Preface reads have an absolute two-second deadline, including idle time and fragmented delivery.
Writes have a two-second socket timeout. Negotiation restores the connection's prior timeout
settings afterward. Stalled handshakes cannot occupy a workspace control worker indefinitely.
There are at most 64 workspace control workers. The manager handles one bounded request per
connection; workspace subscriptions stay open after negotiation.

The protocol version applies to the message schemas as well as the framing. A future incompatible
schema must use a different preface. The JSON parser itself remains useful in-process and does
not consume a socket preface.

## Workspace messages

After negotiation, commands, replies and subscription events are newline-delimited UTF-8 JSON.
Frames are limited to 1 MiB. Request fields and commands are strict; IDs are unsigned integers
and are echoed in replies. For example:

```json
{"command":"list","id":1}
```

Replies have `status`, `id`, and either a result or structured error. The authoritative schemas
are `Request`, `Reply`, and `Event` in `src/control/protocol.rs`. Subscription filters and event
queues are bounded. Pane/viewer IDs are local identifiers, not remote identities or credentials.

The fux CLI performs negotiation itself: commands and `fux ctl` input use ordinary JSON, not the
wire preface. Custom socket clients must negotiate explicitly. Zor's optional observer uses the
same preface before each sampling RPC; it does not link against fux.

## Manager messages

The manager uses the same version preface but a separate strict schema, selected by its socket:

```json
{"request":"list"}
{"request":"resolve","name":"default"}
{"request":"kill","name":"default"}
```

Responses are tagged by `reply`: `pick` carries workspace names, `attach` carries a local descriptor,
and `failed` carries a diagnostic. Descriptors include PID, instance nonce, attachment socket path,
and attachment protocol version. The manager never accepts key files, remote IDs or allowlists.
Killing a workspace deliberately terminates its panes; protocol mismatch never does so implicitly.

## Verification

Runtime tests prove that wrong, missing and partial prefaces reach no command handler, and that a
valid client still works afterward. CLI/fixture tests cover negotiated requests, subscriptions,
workspace switching, simultaneous startup and shutdown. Real zor integration covers independent
consumer negotiation. See `docs/standalone-audit.md` for final verification status.
