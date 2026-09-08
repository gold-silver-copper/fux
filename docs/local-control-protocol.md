# Local control protocol `FUXCTL3`

fux exposes a workspace control socket at `RUNTIME/fux/NAME.sock` and a manager socket at
`RUNTIME/fux/manager.sock`, separate from the length-prefixed attachment socket. The OS user is the
authorization boundary: the server checks the peer's credentials before accepting, and fux's own
clients check the server's.

## Version negotiation

Every connection begins with the eight-byte client preface `FUXCTL3\n`; the server replies with
its own preface, currently `FUXCTL3\n`. Both peers verify equality before sending or dispatching
anything; a missing or different preface executes no command. Preface reads have an absolute
two-second deadline including idle time and fragmentation; writes have a two-second timeout.
There are at most 64 control connections per workspace. A request connection that sends no
complete request for 30 s is closed; a request the server has not answered within 30 s is failed
with its own id. The manager handles one request per connection; workspace subscriptions stay
open until the subscriber sends any byte or closes.

The version covers framing and schemas. `FUXCTL1` (the pre-rewrite schema with popup, hook,
status and observation commands) and `FUXCTL2` (capture without coherent metadata) are not served; an incompatible client is told to use matching
versions or to save work before deliberately restarting the server.

## Command arguments

Configured commands require a non-empty executable name. Empty strings after the executable
are valid positional arguments and are preserved through creation and retained final evidence.
The `new` and `split` APIs also accept an empty argv list to select the configured default.
Argument count, per-argument/total UTF-8 byte limits and NUL rejection still apply.

## Workspace requests

Requests, replies and events are newline-delimited UTF-8 JSON frames of at most 1 MiB. Fields are
strict (`deny_unknown_fields`); `id` is an unsigned integer echoed in the reply. Schemas are
`Request`, `Reply` and `Event` in `src/proto/control.rs`.

Every request accepts an `instance` precondition; input receipt operations, event replay, and
subscriptions with a replay cursor require it. Other operations may omit it. `list` (and workspace listing)
returns `result.value.instance`, the opaque server incarnation generated at startup and also
recorded in workspace descriptors. A controller handle is `(instance, workspace, pane-id)`;
carry that instance on subsequent reads and mutations, including subscriptions. A mismatch
returns `conflict` without executing the operation, even if pane ID and PID happen to repeat.
Discard cached captures and rediscover explicitly after a server change; do not automatically
retry an old mutation against a new incarnation. Ordinary local commands may omit the condition.
The token is an identity precondition, not a secret or authorization credential.

| Command | Fields | Result |
|---|---|---|
| `new` | `cwd?`, `argv?`, `stream?` (require the workspace event-stream lifetime before process creation; mismatch is `conflict`) | `pane` |
| `split` | `axis` (`horizontal`/`vertical`), `target?`, `cwd?`, `argv?` | `pane` |
| `focus` | `target`: `left`/`right`/`up`/`down` or `{"pane":ID}` | unit |
| `kill` | `pane` | unit (the pane leaves the layout now; `pane.closed` follows the exit report) |
| `resize` | `pane`, `delta` (non-zero) | unit |
| `send-keys` | `pane`, `keys` (escapes `\n \r \t \e \\ \0 \xHH`, at most 64 KiB) | unit |
| `input-reserve` | `instance`, `pane` | `input` receipt |
| `input-submit` | `instance`, `operation`, `keys` (same escapes/limit as `send-keys`, nonempty) | `input` receipt |
| `input-status` | `instance`, `operation` | `input` receipt |
| `capture` | `pane`, `attrs?`, `scrollback?` (≤100 000 rows), `max_bytes` (1–131072), `if_revision?` | coherent capture (below) |
| `list` | | `workspaces[]` |
| `tab` | `action`: `new{name?}`, `next`, `previous`, `select{index}`, `select-id{tab}`, `rename{tab,name}`, `close{tab}` | `tab` |
| `workspace` | `action`: `list`, `new{name?}`, `kill{name}` (only the connection's own workspace; other workspaces are killed through the manager or `fux workspace kill`), `select{name}` (viewer attachments only) | `workspace`/`workspaces[]` |
| `events` | `instance`, `after` (`{stream,sequence}`) | `events`: replay batch and current `cursor` |
| `subscribe` | `events?` (≤32 filters), `after?` (requires `instance`) | `accepted`, then sequenced events |

Replies are `{"status":"completed","id":N,"result":{...}}`, `{"status":"failed","id":N,"error":
{"code":"not-found"|"invalid-request"|"limit"|"unknown-command"|…,"message":"…"}}` or
`{"status":"accepted","id":N}` for subscriptions.

Listings carry stable identities: `workspaces[].{name,event_cursor,focused,viewers,tabs[]}`, `tabs[].{id,index,
name,focused,panes[]}`, `panes[].{id,command,pid,cwd,title,progress,geometry,revision,input_sequence,focused,cursor,modes,
exit_status}`. Pane and tab ids are never reused during a server's lifetime; a request naming a
closed id fails with `not-found` even if a replacement exists. Control clients act on the
workspace's own selection, not on any viewer's, and `list`/`capture` never change focus,
selection or a viewport.

Ordering: requests on one connection execute in order and are applied in the same ordered step as
viewer input. A creation reply is sent only after the pane process was started (or failed).
Events are published after the step that produced them, in step order.

## Coherent captures

The result remains `{"kind":"capture","value":{...}}`. Its value contains `text`, `revision`,
`rows`, `columns`, `scrollback_offset`, `title`, `progress`, `unchanged`, `truncated`, and `input_sequence`, all
read from the same emulator state. Dimensions describe content cells, with no border subtraction.
`progress` is null or `[state,percent]`. This is terminal metadata, with no agent interpretation.

Capture returns a viewport of `rows` rows, starting `scrollback_offset` rows above the live
screen. The requested offset is clamped to retained history and a byte-budget-derived limit;
this is not a complete transcript or the entire requested history followed by the live screen.
`truncated` indicates that text was cut at the byte limit. Attribute-preserving text may then
end inside an escape sequence: consumers needing a complete emulated screen must reject it.

`revision` is a change token scoped to a terminal lifetime, advancing on nonempty output and
actual size changes. Reads and unchanged resizes do not advance it. It is not a timestamp,
process status, or count of bytes. Revisions use wrapping u64 arithmetic; compare for equality,
not ordering. A client holding that revision may send `if_revision` to skip text serialization;
the response has `unchanged:true` and empty `text`. It must reuse the same capture options and
cached truncation status, and discard the token when pane/server identity changes. Without a
cached snapshot, always omit `if_revision`. A listing's revision is an invalidation hint;
metadata from a listing must not be combined with text captured in a later RPC.

## Ordered input receipts

Controllers needing retry evidence use three operations, each requiring the current `instance`:

1. `input-reserve` allocates an operation ID without writing input. Persist that ID before submitting.
2. `input-submit` queues decoded bytes once. Repeating the same operation and decoded bytes returns
   its existing receipt; different bytes return `conflict`.
3. `input-status` reads the receipt after reconnecting or while waiting for delivery.

The result is `{"kind":"input","value":{"receipt":{...}}}`. Receipt fields are `operation`,
`pane`, `state`, `revision`, `input_sequence`, `expires_ms`, `bytes_written`, and `error`.
States are `reserved`, `queued`, `delivered`, and `failed`. `delivered` means the PTY writer
accepted all bytes and flushed successfully; it does not establish that the application processed
them. A failure reports known bytes written, including partial writes. Failed operations are never
automatically retried. Losing a reservation reply is safe: reservation itself types nothing.

`input_sequence` counts nonempty controller/viewer input operations. Terminal-generated query
replies do not advance it. The first submission checks that no controller/viewer input intervened
since reservation; otherwise it returns `conflict`. This is an optimistic interference check,
not an exclusive input lease. Accepted submission advances the sequence and records the current
output `revision`. Capture exposes both from the same server step, even for an unchanged revision.
Output after this boundary still needs application-specific interpretation by the controller.

Receipts are retained for 60 seconds from reservation, with at most 128 records across the server;
capacity exhaustion returns `limit` without evicting unexpired records. `expires_ms` uses the
server's monotonic millisecond clock, not wall time. IDs are never reused within an incarnation.
Unknown, wrong-workspace, and expired operations return `expired`: the delivery outcome is unknown.
Expiry removes evidence; it does not cancel queued input. Do not create a replacement operation
automatically after an uncertain outcome. These records are in memory and do not survive server
restart; status also requires an accessible workspace endpoint.

PTY writes are ordered per pane. The pending byte budget is 4 MiB per pane (including the active
write), with at most 1024 queued chunks. Queue rejection becomes a failed tracked receipt with
zero bytes written. Receipt payload retention is separately bounded by 128 × 64 KiB. A blocked
PTY writer does not block the control loop.

## Events

`pane.opened`, `pane.closed` (`exit_status`), `pane.title`, `pane.output`, `tab.opened`,
`tab.closed`, `client.attached`, `client.detached`, and `workspace.changed`. Each published event
contains `cursor:{stream,sequence}` alongside its existing fields and the subscription's `id`.
`workspace.changed` invalidates listing/capture metadata after selection, layout, rename, and
controller/viewer input changes. Events are invalidation hints, not state patches or a transcript.
`pane.output` coalesces output with a 250 ms minimum spacing; a trailing notification is scheduled
for the final update in a burst. Idle panes produce no periodic output events. Capture actual
state after invalidation; never infer application completion from event type or absence.

To synchronize without a registration race:

1. Read `list`, retaining its `instance`, workspace `event_cursor`, and snapshot together.
2. Send `subscribe` with that instance and `after:event_cursor`. Registration precedes the
   authoritative replay read. After `accepted`, replayed events arrive followed by live events;
   entries present in both are emitted once. Replay and live delivery apply the same filters.
3. Retain the last processed cursor. Reconnect using it. A `gap` response before acceptance means
   the interval is unavailable: discard continuity assumptions, re-list, and subscribe again.

For bounded polling, `events` with `after` returns
`{"kind":"events","value":{"cursor":{...},"events":[...]}}`. Its cursor is the boundary of the
read, including an empty batch. Events in that batch have `id:0`; streamed copies have the
subscription ID. The listing's cursor and fields are read during the same ECS request; later
changes are replayed after that boundary. A capture is a separate coherent read and is not made
atomic with an earlier listing by sharing its cursor.

Each workspace lifetime has a unique stream ID within its server incarnation. Sequences increase
without wrapping; a different stream, future sequence, evicted interval, or exhausted sequence
returns `gap`. Recreating a workspace with the same name does not reuse the stream. Streams and
history do not survive server restart or workspace retirement.

Replay retains at most 1024 entries and 512 KiB of serialized event data per workspace, evicting
oldest entries. There is no minimum time guarantee. Each subscriber is separately bounded by
1024 queued events and 512 KiB of serialized data, including the active write. Any overflow ends
the stream; no event type is silently dropped while the stream continues. EOF, a write timeout,
or any other disconnect breaks continuity until replay succeeds. A slow subscriber does not block
the owner loop. Filters intentionally skip other event types, so filtered sequences need not be
consecutive; after a long quiet interval, a filtered subscriber may need a fresh snapshot on
reconnect. Notifications carry no raw terminal byte stream and cannot preserve arbitrary OSC
application reports; those require an application-owned reporting endpoint or a separate justified
generic output transport.

## Manager requests

Same preface, separate strict schema selected by the socket:

```json
{"request":"list"}
{"request":"resolve","name":"default"}
{"request":"resolve","name":null}
{"request":"kill","name":"default"}
{"request":"final","instance":"SERVER_INSTANCE","pane":1}
```

Replies: `{"reply":"names","names":[…]}`, `{"reply":"attach","descriptor":{…}}` (pid, instance
nonce, attachment socket, attachment protocol version) and `{"reply":"failed","message":"…"}`.
`resolve` with `null` applies the default rule: create `default` when nothing exists, otherwise the
most recently attached workspace. `kill` deliberately terminates that workspace's panes; a version
mismatch never does by itself (the interactive viewer may offer to stop an older server, but only
after the operator confirms).

## Retained final evidence

`fux final --instance SERVER_INSTANCE PANE` uses the manager socket, so it works after the pane's
workspace socket disappears. The manager request above returns `{"reply":"final","result":REPLY}`;
the CLI prints the nested control reply directly and exits unsuccessfully for a failed reply.
Successful control replies contain `{"kind":"final","value":{"record":{...}}}` and `id:0`.

A record contains `pane`, `workspace`, workspace `stream`, launch `command` and `cwd`,
`input_sequence`, `exit_status`, `closed_ms`, `expires_ms`, and `capture`. Identity is scoped to
the required server incarnation; a mismatch returns `conflict`. Pane IDs and stream IDs do not
repeat, so recreating a workspace name cannot replace old evidence. A pane still present in the
server returns `conflict`; use ordinary capture until it is released.

The retained capture is the last observed live viewport, plain text with a 128 KiB limit and the
normal coherent metadata/truncation flags. It is not the complete scrollback or a byte transcript.
A known `exit_status` is process-exit evidence, not proof of application success. Forced workspace
closure, termination deadline, or another release before exit observation leaves `exit_status:null`;
output produced after release is unavailable. Never interpret that record as complete output or a
successful exit. An empty untruncated capture with a known exit is distinguishable from absent evidence.

At most 128 records are retained across the server, for up to 60 seconds after release, with oldest
records evicted first when capacity is reached. There is no guaranteed minimum retention under
capacity pressure. `closed_ms` and `expires_ms` are server monotonic milliseconds. Unknown, evicted,
and expired IDs return `expired`. Reads do not renew retention. The manager stays available after
the last workspace closes until final records expire; a new workspace may be created during that
interval. Explicit server shutdown bypasses retention and exits promptly. Records are in memory:
server crash/restart loses them, and a disconnected manager must not be treated as an empty result.

## Consumers

The fux CLI (`fux [NAME] list`, `fux ctl JSON`, …) negotiates itself and takes plain JSON. zor's
`observe` command negotiates `FUXCTL3` before each sampling request and consumes `list` and
`capture` directly. The fixture-child suite and `tools/xtask/src/scenarios/rejection.rs` prove wrong,
missing and partial prefaces reach no handler while a valid client keeps working.


Closing a tab removes it from the visible tab list immediately, while its internal ownership
record remains until its last terminating pane is released. This preserves workspace/stream
identity for retained final evidence. Closed tabs cannot be selected or renamed, and terminating
panes reject input. Final output can retain an unknown exit status when workspace retirement
releases handles before the process exit report arrives; no successful exit is inferred.
