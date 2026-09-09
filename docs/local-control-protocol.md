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
| `split` | `axis` (`horizontal`/`vertical`), `target?`, `cwd?`, `argv?`, `env?`, `rows?`, `columns?` | `pane` |
| `focus` | `target`: `left`/`right`/`up`/`down` or `{"pane":ID}` | unit |
| `kill` | `pane` | unit (the pane leaves the layout now; `pane.closed` follows the exit report) |
| `resize` | `pane`, `delta` (non-zero) | unit |
| `send-keys` | `pane`, `keys` (at most 64 KiB), `notation?` (`escapes` default, or `keys`) | unit |
| `capture` | `pane`, `attrs?`, `scrollback?` (≤100 000 rows), `max_bytes` (1–131072), `format?` (`text` default, `rows`), `since?` (grid sequence; `rows` only, no scrollback), `if_revision?` (text only) | coherent `capture`, or `rows` (below) |
| `list` | | `workspaces[]` |
| `info` | | `info`: `pid`, `instance_nonce`, `version`, `runtime_dir`, `workspace`, `limits{…}` |
| `wait` | `pane`, `until`, `timeout_ms` (1–300000) | `waited`: `fired`, `seq`, `exit_status` |
| `tab` | `action`: `new{name?}`, `next`, `previous`, `select{target}` (`{index:N}` or `{id:TAB}`), `rename{tab,name}`, `close{tab}` | `tab` |
| `workspace` | `action`: `list`, `new{name?}`, `kill{name}` (only the connection's own workspace; other workspaces are killed through the manager or `fux workspace kill`), `select{name}` (viewer attachments only) | `workspace`/`workspaces[]` |
| `input-reserve` | `instance`, `pane` | input receipt |
| `input-submit` | `instance`, `operation`, `keys` (escape notation) | input receipt |
| `input-status` | `instance`, `operation` | input receipt |
| `events` | `instance`, `after` | current cursor and retained events |
| `subscribe` | `events?` (≤32 filters), `after?` (requires `instance`) | `accepted`, replay, then live events |

Replies are `{"status":"completed","id":N,"result":{...}}`, `{"status":"failed","id":N,"error":
{"code":"not-found"|"invalid-request"|"limit"|"unknown-command"|…,"message":"…"}}` or
`{"status":"accepted","id":N}` for subscriptions.

Listings carry stable identities: `instance`, `workspaces[].{event_cursor,name,focused,viewers,tabs[]}`, `tabs[].{id,index,
name,focused,panes[]}`, `panes[].{id,command,pid,cwd,title,progress,seq,revision,input_sequence,geometry,focused,
cursor,modes,exit_status}`. Pane and tab ids are never reused during a server's lifetime; a request naming a
closed id fails with `not-found` even if a replacement exists. Control clients act on the
workspace's own selection, not on any viewer's, and `list`/`capture` never change focus,
selection or a viewport.

Ordering: requests on one connection execute in order and are applied in the same ordered step as
viewer input. A creation reply is sent only after the pane process was started (or failed).
Events are published after the step that produced them, in step order.

## Identity, captures and tracked input

Every workspace request accepts `instance`, the server incarnation from `list` or `info`.
A supplied identity must match before the operation executes. Tracked input, replay and
conditional text capture require it. Workspace `event_cursor.stream` identifies a workspace
lifetime independently of its reusable name; scoped `split` and workspace `kill` accept
`stream` with `instance` to reject a replacement workspace.

Text capture returns `seq`, `input_sequence`, `text`, `revision`, `rows`, `columns`,
`scrollback_offset`, `title`, `progress`, `unchanged` and `truncated` from one coherent read.
`scrollback` selects a viewport shifted into retained history; it does not append a transcript.
`if_revision` with matching revision returns metadata and empty text with `unchanged: true`.
Reuse the cached text only with the same pane/server identity and capture options. For an
unchanged response retain the cached truncation flag. Nonempty output conservatively advances
terminal revision even when the refreshed grid sequence does not change; actual resize also
advances revision. Grid sequence, terminal revision, input sequence and replay cursor are
separate contracts.

Reserve input to obtain an operation and the pane's input sequence, then submit escaped keys
using that operation. Intervening application input causes a conflict before first submission.
Submitting identical bytes again returns the existing receipt without repeating the input;
different bytes conflict. Terminal host replies do not count as intervening application input.
Receipts contain `state` (`reserved`, `queued`, `delivered`, `failed`), `bytes_written`, `error`,
`revision`, `input_sequence` and server-clock `expires_ms`. Delivery means PTY write completion,
not application acknowledgement. A failed write may have delivered a prefix. An expired or
unavailable receipt leaves the outcome unknown; do not automatically replay the input.
At most 128 receipts are retained for 60 seconds; expiry does not cancel queued bytes.

## Creating panes and sending keys

Pane `split` accepts `env` (an array of `[name, value]` pairs, at most 64 entries and 16 KiB
total, applied on top of the sanitized inherited environment) and `rows`/`columns` for the pane's
initial spawn size. Subsequent layout can resize the pane, including without a viewer;
attached viewers determine the tab's available area. Workspace creation does not accept these
fields in either the manager or workspace control schema.

`send-keys` reads its payload in one of two notations. `escapes` (the default) is byte-exact with
`\n \r \t \e \\ \0 \xHH`. `keys` reads space-separated key names: `Enter`, `Tab`, `Escape`,
`Space`, `Backspace`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`,
`Delete`, `F1`-`F12`, `C-<key>` (control), `M-<key>` (meta, an `Escape` prefix), or a single
literal character; arrow and navigation keys send their normal-cursor-mode sequences.

## Output sequence

Every pane has an output sequence `seq`: a counter that advances once for each change an observer
can see (visible rows, cursor, terminal modes, title or exit status), never for output that
changes nothing. It is reported by `list`, by `capture` (the value the returned text or rows
reflect) and by `pane.output` events, and a client that remembers the sequence it last read can
ask for only what changed since:

```json
{"command":"capture","id":4,"pane":1,"max_bytes":65536,"format":"rows","since":17}
{"status":"completed","id":4,"result":{"kind":"rows","value":{"seq":19,"cursor":{"row":3,"column":0,"hidden":false},
  "rows":[{"row":2,"text":"$ make","wrapped":false},{"row":3,"text":"","wrapped":false}],"since_applied":true}}}
```

`rows` lists visible rows top to bottom with trailing blanks trimmed and wide characters as one
entry; without `since` every visible row is listed and `since_applied` is `false`. A resize
re-stamps every row, so the next `since` capture returns the whole screen. `since` with
`scrollback` or with the `text` format is `invalid-request` (history rows carry no sequence), as
is `attrs` with `rows`. `max_bytes` bounds the total row text; rows past it are dropped whole.
The sequence is current at the moment of the reply: a hidden pane's screen is read when it is
listed, captured or its output event is due, a shown pane's whenever a viewer's frame goes out.

## Waiting

`wait` blocks a request until a pane meets a condition or the timeout elapses, so an agent need
not poll. It is a server-side deadline, never a held thread, and the reply says which condition
fired with the pane's current `seq` and `exit_status`:

- `{"kind":"quiet","ms":M}` — no observable change (the output sequence did not advance) for `M` ms.
- `{"kind":"pattern","regex":R}` — the visible screen's plain text matches `R` (a linear-time
  regex, at most 512 bytes).
- `{"kind":"exit"}` — the pane's process exits (the reply carries `exit_status`).
- `{"kind":"seq","value":V}` — the pane's output sequence reaches `V`.

```json
{"command":"wait","id":8,"pane":1,"until":{"kind":"pattern","regex":"\\$ $"},"timeout_ms":10000}
{"status":"completed","id":8,"result":{"kind":"waited","value":{"fired":"pattern","seq":31,"exit_status":null}}}
```

A pane that closes fails every wait on it with `not-found`; a viewer that disconnects drops its
waits. The timeout is a `failed` reply with code `timeout`, never a hang. A server holds at most
1,024 pending waits, at most 64 on one pane; `timeout_ms` and `quiet` `ms` are 1–300000. A
viewer's waits are dropped when it detaches; a control-connection wait that outlives its client
is bounded by its own timeout (there is no separate close signal on the control socket).

## Events

Generic events are `workspace.changed`, `pane.opened`, `pane.closed` (`exit_status`),
`pane.title`, `pane.output` (`seq`), `tab.opened`, `tab.closed`, `client.attached`, and
`client.detached`. Each event carries a cursor containing workspace-lifetime `stream`
and replay `sequence`. Subscription delivery uses the subscription's `id`; records
returned by the `events` RPC retain their stored IDs.

`pane.output` retains the grid sequence semantics. Output that changes only capture
history or metadata emits `workspace.changed` instead. These output invalidations share
one per-pane pacing interval (250 ms), including the last pending update after a burst.
Nonempty no-op output may conservatively invalidate capture revision; it does not advance
the grid sequence. Empty output and idle panes generate no periodic invalidations.
Actual geometry changes, focus and tab changes also invalidate workspace snapshots.

Each workspace retains at most 1024 events and 512 KiB of event data. `events` and
`subscribe` with `after` require the server `instance` and reject evicted cursors or
replaced workspace streams with an explicit `gap`. Rediscover the listing and its
`event_cursor` after a gap. Subscriber count/byte overflow disconnects the slow client;
reconnect with the last accepted cursor to replay, subject to the same retention limits.

## Terminal metadata and ownership

fux exposes generic terminal title and progress metadata. Agent interpretation, provider
integration, task state, checks and verified results belong to zor. fux ignores OSC 7877
agent reports and exposes no pane agent state or `pane.agent` event. Terminal output,
input delivery and process exit are observations, not verified task completion.

## Manager requests

Same preface, separate strict schema selected by the socket:

```json
{"request":"list"}
{"request":"resolve","name":"default"}
{"request":"resolve","name":null}
{"request":"create","name":"new-workspace"}
{"request":"final","instance":"SERVER_NONCE","pane":1}
{"request":"kill","name":"default"}
{"request":"info"}
```

Replies: `{"reply":"names","names":[…]}`, `{"reply":"attach","descriptor":{…}}` (name, pid,
instance nonce, attachment socket path), `{"reply":"info","info":{…}}` (the same `info` the
workspace socket returns, with `workspace` null) and `{"reply":"failed","message":"…"}`. The
manager uses a separate bootstrap schema because its attach reply
carries a descriptor (socket paths) the shared control schema deliberately does not.
`resolve` with `null` applies the default rule: create `default` when nothing exists, otherwise the
most recently attached workspace. `kill` deliberately terminates that workspace's panes; nothing else does.

`create` creates only when the name is absent; it never attaches to an existing workspace.
`final` returns a `final` manager envelope containing a control reply. A live pane returns
`pending`; a retained record supplies immutable final capture, original workspace identity,
command/cwd and exit evidence. Records are bounded to 128 entries and up to 60 seconds (capacity pressure evicts older records), with at
most 128 KiB of capture text each. Forced retirement may leave exit status unknown; late
reports do not rewrite published records. The manager can remain alive after workspace
sockets disappear to serve these records. The CLI exposes `fux final --instance NONCE PANE`;
`fux run` uses this evidence and never claims task success from PTY delivery alone.

## Consumers

The fux CLI (`fux [NAME] list`, `fux ctl JSON`, …) sends the preface itself and takes plain JSON.
zor's `observe` command sends the preface before each sampling request and consumes `list` and
`capture` directly. The fixture-child suite covers bounded control framing; the Rust
`observer` scenario checks that malformed control clients leave panes and valid clients
working. The Rust `protocol-rejection` scenario separately verifies that a rejected
attachment hello leaves terminal settings and screen mode untouched.
