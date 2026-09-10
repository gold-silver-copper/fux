# Remote service composition

Expose the fux attachment stream and zor control socket through separate koh gateway
services, each with its own endpoint identity and explicit peer allowlist. Koh carries
opaque bytes and owns remote authentication/retention. Zor interprets its control
requests; fux interprets only its generic attachment protocol.

| Service | Local socket | Protocol | Grant |
|---|---|---|---|
| Multiplexer attachment | fux workspace `.attach.sock` | Fux attachment v6 | Explicit viewer peer IDs |
| Agent controller | zor `control.sock` | Zor service v1 JSON lines | Explicit controller peer IDs |

Example server invocations, substituting the actual private sockets and endpoint IDs:

```sh
koh gateway serve --socket /private/runtime/fux/default.attach.sock \
  --key-file /private/keys/attachment --allow VIEWER_ENDPOINT_ID
koh gateway serve --socket /private/runtime/zor/control.sock \
  --key-file /private/keys/controller --allow CONTROLLER_ENDPOINT_ID
```

Each command reports its own endpoint ID. A client connects to the selected service
using its corresponding client key, exposing a private local proxy socket:

```sh
koh gateway connect CONTROLLER_SERVER_ENDPOINT_ID \
  --key-file /private/keys/controller-client --socket /private/proxy/zor/control.sock
zor status --directory /private/proxy/zor
```

The proxy's parent directory must be private to its owner. Start the actual zor
service on the server first; a remote proxy is not a local observer runtime. Keep
the attachment and control endpoints and proxy sockets distinct. A viewer identity
not listed on the controller gateway must be rejected there before it reaches zor.
No attachment-to-control routing or shared grant is implied by using koh for both.

This separation concerns gateway access. An interactive terminal remains capable of
running commands with the local account's existing authority. It is not a sandbox
against a shell user invoking local zor commands or accessing same-UID sockets.
Likewise, a controller grant permits the documented zor task operations, not merely
read-only observation. Do not describe endpoint separation as OS-account isolation.

## Reconnect contract and current verification

Koh's gateway retains a session for 30 seconds, scoped to the authenticated peer and
session token. Within retention, the existing session can resume its byte streams.
After expiry it rejects that resume rather than recreating the local application.
Application-level uncertainty, task receipts and reconciling a new connection remain
zor's responsibility. The gateway does not supply standalone koh predictive local echo.

The required `references/koh/tests/gateway.rs` test
`attachment_peer_cannot_use_separately_authorized_real_zor_control` launches actual
fux and zor owners, admits a viewer at the attachment endpoint, denies that same peer
at the control endpoint, and requires a separately authorized control peer to receive
a real zor ping reply. Gateway shutdown must preserve both local owners. The combined
gate and CI require both FUX_BIN and ZOR_BIN; the test cannot silently skip there.

The test compiles and passes strict clippy, but local runtime verification currently
fails creating the netmon monitor with EPERM before authorization is exercised.
Real-fux reconnect coverage includes repeated losses within retention and
`real_fux_rejects_resume_after_actual_retention_and_allows_fresh_attachment`, which
waits the production retention interval plus one second without changing timestamps,
requires rejection of the old token without replay, then checks a fresh attachment
retains the same shell PID and next input count. Its local execution also stops at
netmon EPERM before those assertions. Neither static inspection nor compilation
closes these runtime acceptance gaps. See the completion checklist's R6 status.
