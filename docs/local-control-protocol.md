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
fixtures in `crates/fux/tests/verify/fixtures/`, and every consumer (the CLI, the viewer, koh, zor, the
harnesses) ships from the same tree or from a pinned, unpatched commit (`tools/xtask/companions.json`).
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
| `split` | `axis` (`horizontal`/`vertical`), `final_retain_ms` (nonzero; clamped to `final_retention_ms`), `target?`, `cwd?`, `argv?`, `env?`, `rows?`, `columns?` | `pane` |
| `focus` | `target`: `left`/`right`/`up`/`down` or `{"pane":ID}` | unit |
| `kill` | `pane` | unit (the pane leaves the layout now; `pane.closed` follows the exit report) |
| `resize` | `pane`, `delta` (non-zero) | unit |
| `send-keys` | `pane`, `keys` (at most 64 KiB), `notation?` (`escapes` default, or `keys`) | unit |
| `capture` | `pane`, `attrs?`, `scrollback?` (≤100 000 rows; `text` only), `max_bytes` (1–131072), `format?` (`text` default or `cells`), `if_revision?` | coherent `capture` or `cells` (below) |
| `list` | | `workspaces[]` |
| `info` | | `info`: `pid`, `instance_nonce`, `version`, `runtime_dir`, `workspace`, `limits{scrollback_lines,frame_bytes,capture_bytes,key_bytes,input_retention_ms,final_retention_ms}` (the bounds a client sizes requests by and the retention ceilings it chooses under) |
| `tab` | `action`: `new{name?}`, `next`, `previous`, `select{target}` (`{index:N}` or `{id:TAB}`), `rename{tab,name}`, `close{tab}` | `tab` |
| `workspace` | `action`: `list`, `new{name?}`, `kill{name}` (only the connection's own workspace; other workspaces are killed through the manager or `fux workspace kill`), `select{name}` (viewer attachments only) | `workspace`/`workspaces[]` |
| `input-reserve` | `instance`, `pane`, `retain_ms` (nonzero; clamped to `input_retention_ms`) | input receipt |
| `input-submit` | `instance`, `operation`, `keys` (escape notation) | input receipt |
| `input-status` | `instance`, `operation` | input receipt |
| `events` | `instance`, `after` | current cursor and retained events |
| `subscribe` | `after?` (requires `instance`) | `accepted`, replay, then every live event of the workspace |

Replies are `{"status":"completed","id":N,"result":{...}}`, `{"status":"failed","id":N,"error":
{"code":"not-found"|"invalid-request"|"limit"|"unknown-command"|…,"message":"…"}}` (the manager's
`final` adds `pending`, `conflict`, `evicted`, `expired` and `unknown`, below) or
`{"status":"accepted","id":N}` for subscriptions.

Listings carry stable identities: `instance`, `workspaces[].{event_cursor,name,focused,viewers,tabs[]}`, `tabs[].{id,index,
name,focused,panes[]}`, `panes[].{id,command,pid,cwd,title,seq,revision,input_sequence,geometry,focused,
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

`format: "cells"` returns the visible grid as cells instead of text, from the same single read
of the pane: `seq`, `input_sequence`, `revision`, `rows`, `columns`, `cursor`, `title`,
`progress`, `unchanged`, `truncated` and `lines`. A text capture and a cells capture served in
the same step report the same `revision`, `seq` and `input_sequence`, so a consumer can
evaluate screen rules on the cells without re-emulating the text. Each line carries `row`,
`wrapped` and `cells` in the viewer's wire encoding: `{"text":"a"}` is a text cell, `{}` a
blank, `{"run":N}` `N` equal blanks, `kind` is spelled out only for `wide-leading` and
`wide-continuation`, and `style` (`foreground`, `background`, `bold`, `dim`, `italic`,
`underline`, `inverse`) only when it is not the default; a line expands to exactly `columns`
cells. `if_revision` behaves as for text: a matching revision returns the metadata with
`unchanged: true` and no lines. `max_bytes` bounds the JSON encoding of `lines`: lines are
kept whole, top to bottom, while the encoding stays within the bound; the rest are dropped and
`truncated` is `true`. `scrollback` and `attrs` are text-form options and are `invalid-request`
with `cells`.

```json
{"command":"capture","id":5,"pane":1,"max_bytes":65536,"format":"cells"}
{"status":"completed","id":5,"result":{"kind":"cells","value":{"seq":12,"input_sequence":3,"revision":15,
  "rows":2,"columns":8,"cursor":{"row":1,"column":0,"hidden":false},"title":"sh","progress":null,
  "unchanged":false,"truncated":false,"lines":[
    {"row":0,"wrapped":false,"cells":[{"text":"$"},{},{"text":"日","kind":"wide-leading","style":{"foreground":{"Indexed":1},
      "background":"Default","bold":true,"dim":false,"italic":false,"underline":false,"inverse":false}},
      {"kind":"wide-continuation","style":{"foreground":{"Indexed":1},"background":"Default","bold":true,"dim":false,
      "italic":false,"underline":false,"inverse":false}},{"run":4}]},
    {"row":1,"wrapped":false,"cells":[{"run":8}]}]}}}
```

Reserve input to obtain an operation and the pane's input sequence, then submit escaped keys
using that operation. Intervening application input causes a conflict before first submission.
Submitting identical bytes again returns the existing receipt without repeating the input;
different bytes conflict. Terminal host replies do not count as intervening application input.
Receipts contain `state` (`reserved`, `queued`, `delivered`, `failed`), `bytes_written`, `error`,
`revision`, `input_sequence` and server-clock `expires_ms`. Delivery means PTY write completion,
not application acknowledgement. A failed write may have delivered a prefix. An expired or
unavailable receipt leaves the outcome unknown; do not automatically replay the input.
Retention is the caller's policy under a server ceiling: `input-reserve` requires `retain_ms`,
the milliseconds the receipt stays readable from the reservation; `0` is `invalid-request`, a
value above `info.limits.input_retention_ms` (600 000, ten minutes) is clamped to it, and
`expires_ms` is the reservation time plus the value actually applied, so a caller sees the
clamp in the receipt. At most 128 receipts are retained at once, whatever their durations
(capacity is fux's bound against any client); expiry does not cancel queued bytes.

## Creating panes and sending keys

Pane `split` requires `final_retain_ms`, the milliseconds the pane's final record (below) stays
readable after the pane closes: the record is created at exit, when no client need be present,
so the launcher states its retention when it creates the pane. `0` is `invalid-request`; a value
above `info.limits.final_retention_ms` (14 400 000, four hours) is clamped to it. fux's own
panes (a workspace's initial pane, a new tab's pane, the viewer's and CLI's splits) use the
configured `[final] retain-ms` (default 60 000). `split` also accepts `env` (an array of
`[name, value]` pairs, at most 64 entries and 16 KiB total, applied on top of the sanitized
inherited environment) and `rows`/`columns` for the pane's initial spawn size. Subsequent layout can resize the pane, including without a viewer;
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
changes nothing. It is reported by `list`, by `capture` (the value the returned text or cells
reflect) and by `pane.output` events, so a client that remembers the sequence it last read can
tell whether a capture is worth taking; `pane.output` events report it as it moves.
The sequence is current at the moment of the reply: a hidden pane's screen is read when it is
listed, captured or its output event is due, a shown pane's whenever a viewer's frame goes out.

## Events

Generic events are `workspace.changed`, `pane.opened`, `pane.closed` (`exit_status`),
`pane.output` (`seq`), `tab.opened` (`name`) and `tab.closed`. Each event carries a cursor
containing workspace-lifetime `stream` and replay `sequence`. Subscription delivery uses the
subscription's `id`; records returned by the `events` RPC retain their stored IDs. A
subscription receives every event of its workspace; what to act on is the consumer's
selection. Title changes and viewer attachments are not events: a title change advances the
pane's output sequence (`pane.output`) and is read from `list` or a capture, and viewer
counts are read from `list`.

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

fux exposes generic terminal title metadata in `list` and both capture forms, and OSC 9;4
progress only through capture (`text` and `cells`). Agent interpretation, provider
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
`final` returns a `final` manager envelope containing a control reply. A retained record
supplies immutable final capture, original workspace identity, command/cwd and exit evidence.
Each record lives for the `final_retain_ms` its pane was created with (clamped to four hours),
from the moment the pane closed; at most 128 records are retained at once (capacity pressure
evicts the oldest-closed record early), with at most 128 KiB of capture text each. Forced
retirement may leave exit status unknown; late reports do not rewrite published records. The
manager can remain alive after workspace sockets disappear to serve these records.

A `final` without a record fails with one of five codes, and only the first is worth polling:

| code | meaning |
|---|---|
| `pending` | the pane is still live; use `capture` for current evidence |
| `conflict` | `instance` is not this server; the evidence belonged to a previous server |
| `evicted` | the record existed and the 128-record cap dropped it under load before its `expires_ms` |
| `expired` | the record existed and its retention elapsed |
| `unknown` | this server never retained a record for that pane id, or has since forgotten that it did |

fux distinguishes the last three without retaining more evidence: per server instance it keeps
two bounded rings of pane ids, the ids evicted by the cap and the ids whose retention elapsed,
each holding the most recent 1024 ids (`MAX_EVICTED_FINAL_IDS`); the oldest id is dropped when a
ring is full. The exact rule: `evicted` while the id is in the eviction ring; `expired` while the
record is still present past its `expires_ms` or the id is in the expiry ring; `unknown`
otherwise. An id is never reported `expired` or `evicted` unless a record was made for it;
after more than 1024 later evictions or expiries an id's history is forgotten and it becomes
`unknown`, so a consumer that reads late must treat `unknown` as lost evidence, not as proof
that the pane never existed. `final` is the primitive; the workflows over it
(`zor run`, zor's managed launches) live in zor and never claim task success from PTY
delivery alone. The fux CLI has no subcommand for it.

## Consumers

The fux CLI (`fux [NAME] list`, `fux ctl JSON`, …) sends the preface itself and takes plain JSON.
zor's `observe` command sends the preface before each sampling request and consumes `list` and
`capture` directly; `zor run` is the one-shot workflow over `create`, `split`, `final` and
workspace `kill`. The fixture-child suite covers bounded control framing; the Rust
`observer` scenario checks that malformed control clients leave panes and valid clients
working. The Rust `protocol-rejection` scenario separately verifies that a rejected
attachment hello leaves terminal settings and screen mode untouched.
