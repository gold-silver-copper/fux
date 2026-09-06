# Local control protocol `FUXCTL2`

fux exposes a workspace control socket at `RUNTIME/fux/NAME.sock` and a manager socket at
`RUNTIME/fux/manager.sock`, separate from the length-prefixed attachment socket. The OS user is the
authorization boundary: the server checks the peer's credentials before accepting, and fux's own
clients check the server's.

## Version negotiation

Every connection begins with the eight-byte client preface `FUXCTL2\n`; the server replies with
its own preface, currently `FUXCTL2\n`. Both peers verify equality before sending or dispatching
anything; a missing or different preface executes no command. Preface reads have an absolute
two-second deadline including idle time and fragmentation; writes have a two-second timeout.
There are at most 64 control connections per workspace. A request connection that sends no
complete request for 30 s is closed; a request the server has not answered within 30 s is failed
with its own id. The manager handles one request per connection; workspace subscriptions stay
open until the subscriber sends any byte or closes.

The version covers framing and schemas. `FUXCTL1` (the pre-rewrite schema with popup, hook,
status and observation commands) is not served; an incompatible client is told to use matching
versions or to save work before deliberately restarting the server.

## Workspace requests

Requests, replies and events are newline-delimited UTF-8 JSON frames of at most 1 MiB. Fields are
strict (`deny_unknown_fields`); `id` is an unsigned integer echoed in the reply. Schemas are
`Request`, `Reply` and `Event` in `src/proto/control.rs`.

| Command | Fields | Result |
|---|---|---|
| `new` | `cwd?`, `argv?` | `pane` |
| `split` | `axis` (`horizontal`/`vertical`), `target?`, `cwd?`, `argv?` | `pane` |
| `focus` | `target`: `left`/`right`/`up`/`down` or `{"pane":ID}` | unit |
| `kill` | `pane` | unit (the pane leaves the layout now; `pane.closed` follows the exit report) |
| `resize` | `pane`, `delta` (non-zero) | unit |
| `send-keys` | `pane`, `keys` (escapes `\n \r \t \e \\ \0 \xHH`, at most 64 KiB) | unit |
| `capture` | `pane`, `attrs?`, `scrollback?` (≤100 000 rows), `max_bytes` (1–131072) | `text` |
| `list` | | `workspaces[]` |
| `tab` | `action`: `new{name?}`, `next`, `previous`, `select{index}`, `select-id{tab}`, `rename{tab,name}`, `close{tab}` | `tab` |
| `workspace` | `action`: `list`, `new{name?}`, `kill{name}` (only the connection's own workspace; other workspaces are killed through the manager or `fux workspace kill`), `select{name}` (viewer attachments only) | `workspace`/`workspaces[]` |
| `subscribe` | `events?` (≤32 filters) | `accepted`, then events |

Replies are `{"status":"completed","id":N,"result":{...}}`, `{"status":"failed","id":N,"error":
{"code":"not-found"|"invalid-request"|"limit"|"unknown-command"|…,"message":"…"}}` or
`{"status":"accepted","id":N}` for subscriptions.

Listings carry stable identities: `workspaces[].{name,focused,viewers,tabs[]}`, `tabs[].{id,index,
name,focused,panes[]}`, `panes[].{id,command,pid,cwd,title,progress,geometry,focused,cursor,modes,
exit_status}`. Pane and tab ids are never reused during a server's lifetime; a request naming a
closed id fails with `not-found` even if a replacement exists. Control clients act on the
workspace's own selection, not on any viewer's, and `list`/`capture` never change focus,
selection or a viewport.

Ordering: requests on one connection execute in order and are applied in the same ordered step as
viewer input. A creation reply is sent only after the pane process was started (or failed).
Events are published after the step that produced them, in step order.

## Events

`pane.opened`, `pane.closed` (`exit_status`), `pane.title`, `pane.output` (rate-limited per pane),
`tab.opened`, `tab.closed`, `client.attached`, `client.detached`. Each event carries the
subscription's `id`. A subscriber whose queue exceeds 1024 events is disconnected rather than
buffered without bound; it may resubscribe and re-`list`.

## Manager requests

Same preface, separate strict schema selected by the socket:

```json
{"request":"list"}
{"request":"resolve","name":"default"}
{"request":"resolve","name":null}
{"request":"kill","name":"default"}
```

Replies: `{"reply":"names","names":[…]}`, `{"reply":"attach","descriptor":{…}}` (pid, instance
nonce, attachment socket, attachment protocol version) and `{"reply":"failed","message":"…"}`.
`resolve` with `null` applies the default rule: create `default` when nothing exists, otherwise the
most recently attached workspace. `kill` deliberately terminates that workspace's panes; a version
mismatch never does by itself (the interactive viewer may offer to stop an older server, but only
after the operator confirms).

## Consumers

The fux CLI (`fux [NAME] list`, `fux ctl JSON`, …) negotiates itself and takes plain JSON. zor's
`observe` command negotiates `FUXCTL2` before each sampling request and consumes `list` and
`capture` directly. The fixture-child suite and `tests/verify/protocol_rejection.py` prove wrong,
missing and partial prefaces reach no handler while a valid client keeps working.
