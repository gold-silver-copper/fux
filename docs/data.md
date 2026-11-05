# Grift data from fux

fux prints records as association lists. `read-form` reads that data without evaluating it.
A program asks for a field with `(get value "field")`; a handle asks the running server,
while a held record or capture returns its retained observation.

```grift
(define! held (information (pane 1)))
(get (first (get held "panes")) "columns")
```

Absent data is `none`, distinct from the empty list `()`. Booleans are `#true` and `#false`.
Field and error names use hyphens, for example `layout-revision`, `process-id`, and `no-pane`.
The files `records.txt` and `words.txt` list the interface from its actual builders.

Captures print terminal text by default. `(datum capture)` exposes the capture as records
and lists. `(data-text value)` returns a string of readable grift data; a large retained
capture is serialized one row at a time. These operations read no live terminal facts again.
Styles are rows of runs with `column`, `length`, and the attributes each run sets.
The captured `lines`, `screen-cursor`, and optional `changed-rows` belong to that same read.
A client capture has `client`, `screen-rows`, and `screen-columns`, with no pane version.

A final list streams one item per line. `display` writes a whole value as one datum.
`next-events` returns a held batch; the `events` field contains the records, `missed` tells
whether the journal lost entries, and `next` is a cursor for the next request.
Saved sessions are one grift program over a quoted recipe. Restore with `fux --file FILE`.

Errors are grift contexts with `kind`, `values`, and `trace`. Catch by kind, for example
`(catch-kind! (kill (pane 999)) no-pane failure #inert)`. Diagnostics go to stderr.
The native transport is protocol 77; the command language and printed data use grift.
Native error kinds:

`cancelled`, `no-server`, `mismatch`, `client`, `no-pane`, `no-tab`, `no-workspace`, `no-client`, `no-target`, `stray-pane`, `no-field`, `nothing-there`, `name-taken`, `bad-value`, `usage`, `not-running`, `still-running`, `not-run`, `only-shell`, `stale`, `no-marks`, `start`, `size`, `mouse`, `signal`, `indeterminate`, `not-headless`, `own-server`, `people-attached`, `timeout`, `closed`, `not-here`, `not-config`, `config`, `bad-binding`, `read`, `unset-variable`, `output`, `nested`, `not-a-key`, `type-error`, `range-error`, `arity-error`, `too-long`, `unused`, `unbound-symbol`, `bad-name`, `out-of-memory`, `ids-exhausted`, `fail`, `read-error`, `contract-error`, `parse-error`.

The journal keeps its last `journal-entries` entries (16,384 by default).

The finite coverage schema is:

| Event | Meaning |
| --- | --- |
| `attached` | A native observed event. |
| `bell` | A native observed event. |
| `closed` | A native observed event. |
| `command-started` | A native observed event. |
| `created` | A native observed event. |
| `detached` | A native observed event. |
| `done` | A native observed event. |
| `exited` | A native observed event. |
| `history-budget` | A native observed event. |
| `history-cleared` | A native observed event. |
| `hung-up` | A native observed event. |
| `input` | A native observed event. |
| `laid-out` | A native observed event. |
| `moved` | A native observed event. |
| `notification` | A native observed event. |
| `read-failed` | A native observed event. |
| `renamed` | A native observed event. |
| `reordered` | A native observed event. |
| `resized` | A native observed event. |
| `respawned` | A native observed event. |
| `screen` | A native observed event. |
| `shown` | A native observed event. |
| `signalled` | A native observed event. |
| `stopped` | A native observed event. |
| `title` | A native observed event. |
| `view-resized` | A native observed event. |
| `zoomed` | A native observed event. |

| `status` | Meaning |
| --- | --- |
| `continuing` | More events can arrive. |
| `timeout` | No event arrived within the interval. |
| `gone` | The scope closed. |
| `stopped` | The server stopped. |
