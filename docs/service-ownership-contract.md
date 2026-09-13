# fux, koh and zor service ownership

The composed system has one owner for each resource. Fux owns local pane PTYs,
process groups, terminal state/history, layouts, viewers and generic process evidence.
Zor owns tasks, provider interpretation, supervision, worktrees, artifacts and recovery
policy. Koh owns remote identity, endpoint authorization, encrypted connectivity and
bounded transport reconnect/resume state. `local-ipc` owns private local sockets,
peer credentials and generic bounded framing/deadlines.

Zor may spawn provider adapters and check subprocesses and manage its own dashboard
terminal. Its optional `wrap` feature owns a standalone wrapper PTY. Koh's explicit
shell product owns its standalone PTYs and terminal state. Neither exception grants
ownership of a fux pane in the composed system.

## Identity and routing

| Reference | Meaning | Authority |
|---|---|---|
| Local runtime/service path | Where a connection can be attempted | Location only; authenticate the peer |
| Koh endpoint identity | Authenticated remote transport principal | Permission to connect to the specifically authorized service |
| Fux instance nonce | A particular running fux server | Bound to subsequent pane/workspace requests |
| Workspace name and stream | Current workspace route and incarnation | A route can change while the process remains the same |
| Fux instance, pane and PID with origin workspace/stream | Exact process and immutable launch attribution | Task policy validates it before destructive operations |
| Zor service instance and retained task/attempt identity | Application state and reconciliation context | Zor decides whether an operation remains justified |

Zor's local `fux::endpoint::Endpoint` owns manager/workspace control-socket naming.
Typed client operations validate wire envelopes and required identities. Callers retain
resource authority checks. Discovering a replacement server releases stale ownership;
it does not authorize adopting or stopping its processes. A relocated pane is found
through the manager under its original instance/pane identity, then validated against
its retained origin before using the current workspace route.

## Remote supervision contract

On each machine, local zor talks to local fux. A future remote controller talks to that
machine's zor service through an independently authorized koh service connection.
The controller must retain the service instance and task/attempt identities needed to
reconcile application state. It must not treat a forwarded socket path or a reconnected
QUIC connection as proof that the same application instance still exists.

Attachment and zor control are separately configured and authorized services. Permission
to use one does not imply permission to use the other. An authorized interactive terminal
can execute commands; attachment access is not an observation-only security sandbox.

Koh forwards opaque bytes. It does not parse fux commands, infer task state, replay prompts,
create panes or restart agents. Resuming a retained connection follows koh's bounded byte
resume contract. Expired transport state must be reported, after which zor/controller
policy reconciles application evidence before deciding whether to open a fresh connection
or submit any new operation. Transport reconnection cannot authorize application replay.

This document specifies ownership for future remote supervision. It does not claim that
a multi-machine controller, remote zor client API or distributed scheduler is implemented.
The currently exercised integration is separate attachment/control forwarding.

## Evidence and failures

Fux input receipts describe delivery of bytes under the terminal contract. Provider/native
acknowledgments describe agent acceptance. Zor checks and retained evidence establish task
success. These are separate transitions; neither delivered bytes nor a process exit alone
proves a task succeeded. Generic final records and event cursors belong to fux; their task
interpretation and retention policy belong to zor.

A malformed reply, stale identity, unavailable connection, explicit remote refusal and
pending evidence must remain distinguishable. Losing a mutation reply leaves an unknown
outcome. Reconcile its retained operation/resource identity before considering another
mutation. Workspace creation is create-only where ownership requires a fresh resource;
resolving an existing name does not confer ownership.

RPC operations use an absolute caller deadline across connect, negotiation, write and
complete-frame reads. A slow peer cannot renew that deadline by sending small chunks.
Subscriptions may remain idle indefinitely, but a partially received head frame must finish
within its fixed two-second deadline. Already complete buffered frames are not partial
frames and do not expire merely while other peers are serviced.

## Verification and publication

`fux-xtask verify-boundaries` checks resolved production dependencies for fux and zor across
Linux, macOS and Android feature configurations. `--koh CHECKOUT` adds strict gateway-only
and standalone-shell checks for a koh checkout that supports those features. It does not
relax or replace companion provenance validation. Source-surface guards, independently
checked wire fixtures and real-process tests provide complementary evidence.

Ordinary CI has a distinct composition job using the exact clean companion pin and the
fux/zor binaries built in that run, with required-binary flags. Standalone fux verification
and packaging remain independent of koh. The published koh pin is
`da712875e4f527b718abe44e9d68f94048e916c7`. Gateway composition uses `--no-default-features
--features cli,gateway` and `verify-boundaries --koh references/koh`; separate default-shell
tests remain. The reference checkout must match the exact published pin and remain clean.

A workflow job is not proof of configured branch protection or passing hosted CI. Report
publication, pin updates and hosted verification separately from local development results.
