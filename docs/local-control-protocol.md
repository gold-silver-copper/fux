# Local control protocol

fux exposes a workspace control socket at `RUNTIME/fux/NAME.sock` and a manager socket at
`RUNTIME/fux/manager.sock`, separate from the length-prefixed attachment socket. The OS user is the
authorization boundary: the server checks the peer's credentials before accepting, and fux's own
clients check the server's.

## Preface

Every connection begins with the four-byte client preface `FUX\n`; the server replies with the
same bytes. Both peers verify equality before sending or dispatching anything; a missing or
different preface executes no command. The preface is a magic that keeps a stray connection from
something that is not fux away from the handlers; it is not a version, and nothing in the
protocol is versioned (see "Compatibility" below). Preface reads have an absolute
two-second deadline including idle time and fragmentation; writes have a two-second timeout.
There are at most 64 control connections per workspace. A request connection that sends no
complete request for 30 s is closed; a request the server has not answered within 30 s is failed
with its own id. The manager handles one request per connection; workspace subscriptions stay
open until the subscriber sends any byte or closes.

## Compatibility

There is no protocol versioning. The schemas are whatever the current tree defines, pinned by the
fixtures in `tests/verify/fixtures/`, and every consumer (the CLI, the viewer, koh, zor, the
harnesses) ships from the same tree or from a pinned base plus a patch in `dependency-patches/`.
A request the server does not know fails with `unknown-command`; a field it does not know fails
with `invalid-request`; a reply the client does not understand is reported as an error naming
the session server. A server older than its client is therefore visible as such an error, and the
operator restarts it deliberately; nothing is ever stopped automatically.

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

Replies: `{"reply":"names","names":[…]}`, `{"reply":"attach","descriptor":{…}}` (name, pid,
instance nonce, attachment socket path) and `{"reply":"failed","message":"…"}`.
`resolve` with `null` applies the default rule: create `default` when nothing exists, otherwise the
most recently attached workspace. `kill` deliberately terminates that workspace's panes; nothing else does.

## Consumers

The fux CLI (`fux [NAME] list`, `fux ctl JSON`, …) sends the preface itself and takes plain JSON.
zor's `observe` command sends the preface before each sampling request and consumes `list` and
`capture` directly. The fixture-child suite and `tests/verify/protocol_rejection.py` prove wrong,
missing and partial prefaces reach no handler while a valid client keeps working.
