# What `--json` prints

(`fux help json`.) Every shape below is part of fux's interface: a field is
added, never renamed or dropped without a release saying so. Rows and
columns count from 0; times are milliseconds; ids are strings (`"%3"`,
`"@2"`, `"+1"`, `"c1"`).

## A pane

`ls --json` gives each pane these, and `capture-pane --json` the same:

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
| `revision` | grows with every change to the screen: `wait --change --since N`, `send-keys --if-revision N` |
| `command_running` | a shell command runs (OSC 133 marks) |
| `last_status` | the last shell command's status, else `null` |
| `bells` | bells and notifications rung since it started |
| `notification` | the last notification's text, else `null` |
| `cwd`, `argv` | the foreground process's directory and words, where the system says |

## `ls --json`

```json
{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main","panes":[PANE…]}]}],
 "clients":[{"id":"c1","rows":24,"cols":80,"workspace":"+1","tab":"@1","pane":"%1","zoom":false}],
 "grants":[{"socket":"/home/me/.cache/agent.sock","workspace":"agent"}]}
```

`ls -t TARGET --json` prints that pane, tab (`{"id","name","panes"}`) or
workspace (`{"id","name","tabs"}`) alone; `ls -t TARGET --get FIELD` one of
its fields, a string as it is, anything else as JSON.

## `capture-pane --json`

```json
{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":["…"],PANE FIELDS…}
```

`cursor` is `null` while hidden or outside the rows printed. With
`--styles`, `"styles"` holds a list per line of its runs of cells that are not
plain: `{"col":0,"len":3,"fg":1,"bg":"#1e1e20","bold":true,"dim":true,
"italic":true,"underline":true,"blink":true,"inverse":true,
"hidden":true,"strikeout":true}`, each attribute only when set; colours are
palette numbers or `"#rrggbb"`.

`capture-client --json`:
`{"client":"c1","rows":24,"cols":80,"cursor":[ROW,COL],"lines":["…"]}`.

## What a command made

`split`, `new-tab`, `new-workspace` and `run` with `--json`:
`{"workspace":"+2","tab":"@3","pane":"%4"}`. `layout --json`: an array, a
line each, of the ids it made: `[["+2","@3","%4"],["%5"]]`.

## Waits

`wait --json`, and `--json` with any `--wait-…`: one object, or a line each
for send-keys to several panes.

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

`status` is the wait's exit status; `screen`, with `--print`, the screen.

## `events --json`

One object a line: `{"event":NAME,"pane":"%3","tab":"@2","workspace":"+2",…}`,
`pane` and `tab` only where they apply. `bell`, `notification` and `done`
that came several times in one round of the server's carry `count`.

| `event` | Also |
| --- | --- |
| `ready` | first: nothing is missed from here on |
| `gone` | last: everything `-t` named has closed (status 4) |
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

With `--json`, any command's failure is one object on stderr, and its exit
status is as without:

```json
{"error":"no_pane","message":"no pane %99"}
```

`error` is a code that stays: `usage`, `no_server`, `mismatch`, `no_pane`,
`no_tab`, `no_workspace`, `no_target`, `stray_pane`, `exited`,
`still_running`, `not_run`, `changed`, `own_server`, `people_attached`,
`not_granted`, `name_taken`, `bad_name`, `ambiguous`, `size`, `mouse`,
`no_field`, `layout`, `id`, `config`, `not_reading`, `start`, `paste`, and a
few more for rarer failures.

## `status --json`, `servers --json`

```json
{"running":true,"socket":"/run/user/1000/fux/server.sock","version":"0.22.0","protocol":4,
 "server_version":"0.22.0","server_protocol":4,"compatible":true}
[{"name":"test","socket":"/run/user/1000/fux/test.sock","version":"0.22.0","compatible":true}]
```

With no server, `status` gives `running`, `socket`, `version` and `protocol`
alone, and exits 1.
