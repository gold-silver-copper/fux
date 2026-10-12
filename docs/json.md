# What `json` gives

(`fux help-json`.) `json` turns the value on top of the stack into JSON:
what a word left, as below; a string as a JSON string; a number or boolean
as itself. Every shape below is part of fux's interface: a field is added,
never renamed or dropped without a release saying so. Rows and
columns count from 0; times are milliseconds; ids are strings (`"%3"`,
`"@2"`, `"+1"`, `"c1"`).

## A pane

`ls json` gives each pane these; `PANE opts capture json` gives them too, but
for `id`, `name`, `title` and `pid` (it says `pane`):

| Field | Value |
| --- | --- |
| `id` | `"%3"` |
| `name` | its name: the program's, or what `rename` gave it |
| `title` | what the program set (OSC 0 or 2), else `""` |
| `rows`, `cols` | its size |
| `pid` | its program's process, `null` once it exited |
| `program` | what runs in its foreground, `null` if unknown |
| `paused` | a program a restore brought back, waiting for `respawn` |
| `exited` | the status its program exited with (a pane `run` started), else `null` |
| `signal` | the signal that killed it (its number), else `null` |
| `idle_ms` | how long its screen has shown no change |
| `alt_screen` | its program holds the full screen |
| `mouse` | its program asked for mouse reports |
| `mouse_mode` | which: `"press"`, `"press_release"`, `"button_motion"`, `"any_motion"`, or `null` |
| `bracketed_paste` | its program asked for bracketed paste |
| `pictures` | the pictures it shows (sixel), each at cell `row`, `col`, over `rows` by `cols` cells, `width` by `height` pixels |
| `cursor_visible` | the cursor shows |
| `history_lines` | lines of history it holds |
| `copy_mode` | a client has it in copy mode |
| `revision` | grows with every change to the screen: `keys … N if-revision send-keys` |
| `command_running` | a shell command runs (OSC 133 marks) |
| `last_status` | the last shell command's status, else `null` |
| `bells` | bells and notifications rung since it started |
| `notification` | the last notification's text, else `null` |
| `cwd`, `argv` | the foreground process's directory and words, where the system says |

## `ls json`

```json
{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main","panes":[PANE…]}]}],
 "clients":[{"id":"c1","rows":24,"cols":80,"workspace":"+1","tab":"@1","pane":"%1","zoom":false}],
 "grants":[{"socket":"/home/me/.cache/agent.sock","workspace":"agent"}]}
```

`X info json` gives that pane, tab (`{"id","name","panes"}`) or workspace
(`{"id","name","tabs"}`) alone; `X str FIELD get` leaves one of its fields:
a number or boolean as itself, a string as text, `null` as the text
`null`.

## `PANE opts capture json`

```json
{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":["…"],PANE FIELDS…}
```

`cursor` is `null` while hidden or outside the rows printed. With the
`styles` option, `"styles"` holds a list per line of its runs of cells that are not
plain: `{"col":0,"len":3,"fg":1,"bg":"#1e1e20","bold":true,"dim":true,
"italic":true,"underline":true,"blink":true,"inverse":true,
"hidden":true,"strikeout":true}`, each attribute only when set; colours are
palette numbers or `"#rrggbb"`.

`CLIENT capture-client json`:
`{"client":"c1","rows":24,"cols":80,"cursor":[ROW,COL],"lines":["…"]}`.

## What a word made

`run`, `split`, `new-tab` and `new-workspace` leave the new pane; `json` of
a pane, tab or workspace is its id as a string, `"%4"`, and `X info json`
all of it.

## Waits

`json` of a wait's outcome (`… wait`, or `send-keys`, `send-mouse` given a
wait condition): one object, or a line each for keys sent to several panes.
A wait that fails (`exited`, `closed`, `respawned`, `timeout`, `idle` with
nothing changed) ends the program, its status the wait's, and says why on
stderr: those rows below are what `events`, and its status, tell of it.

```json
{"pane":"%3","met":"text","text":"PASS","line":"12 tests PASS","status":0}
```

| `met` | Also |
| --- | --- |
| `text` | `text`, `line` |
| `gone` | `text` |
| `idle` | `changed` (false: the keys changed nothing; status 5) |
| `change` | `revision` |
| `exit` | `exit` (its status) or `signal` |
| `done` | `exit`: the command's status, else `null` |
| `bell` | `notification`: its text, else `null` |
| `exited` | `exit` or `signal`, `last_lines`: the program exited first (status 3) |
| `respawned` | it was started again first (status 3) |
| `closed` | the pane closed (status 4) |
| `timeout` | `idle_ms`, `last_line` (status 124) |

`status` is the wait's exit status; `screen`, with `with-screen`, the screen.

## `events-json`, `X events-of-json`

One object a line: `{"event":NAME,"pane":"%3","tab":"@2","workspace":"+2",…}`,
`pane` and `tab` only where they apply. `bell`, `notification` and `done`
that came several times in one round of the server's carry `count`.

| `event` | Also |
| --- | --- |
| `ready` | first: nothing is missed from here on |
| `gone` | last: everything `events-of-json` named has closed (status 4) |
| `created`, `renamed` | `name`, for a tab or workspace |
| `closed`, `moved`, `respawned`, `bell` | |
| `exited` | `status` or `signal` |
| `done` | `status`: a shell command's |
| `notification` | `text` (the last of a round) |
| `title` | `title` |
| `resized` | `cols`, `rows` |
| `lost` | last: the reader fell behind (status 1) |
| `stopped` | last: `reason`, the server stopped |

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from. CODE stays: `no_server`, `mismatch`,
`no_pane`, `no_tab`, `no_workspace`, `no_target`, `stray_pane`, `exited`,
`still_running`, `not_run`, `changed`, `own_server`, `people_attached`,
`not_granted`, `name_taken`, `bad_name`, `ambiguous`, `size`, `mouse`,
`no_field`, `id`, `not_reading`, `start`, `paste`, `timeout`, `closed`,
`unchanged`, `gone`, `no_buffer`, `not_a_key`, `no_program`, `no_condition`,
`no_keys`, `not_a_client`, `grant`, `config`, `unused`, `too_long`; and the
language's: `type` (a word given the wrong kind of value), `stack_underflow`,
`unknown_word`, `open_quote`, `unmatched`, `unclosed`, `range`, `bad_value`,
`overflow`, `div_by_zero`, `call_depth`, and a few more for rarer failures.

## `status json`, `servers json`

```json
{"running":true,"socket":"/run/user/1000/fux/server.sock","version":"0.22.0","protocol":5,
 "server_version":"0.22.0","server_protocol":5,"compatible":true}
[{"name":"test","socket":"/run/user/1000/fux/test.sock","version":"0.22.0","compatible":true}]
```

With no server, `status` gives `running`, `socket`, `version` and `protocol`
alone.
