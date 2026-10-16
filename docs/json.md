# What `json` gives

(`fux help-json`.) `json` turns the value on top of the stack into JSON:
what a word left, as below; a string as a JSON string; a number or boolean
as itself; a list as an array of these, each answer in it as `json` gives
it alone (a capture's, a listing's or an action's object; one of several
panes an array of their objects). Every shape below is part of fux's interface: a field is added,
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
| `state` | what it is doing: `reading` (blocked on its terminal, every key read), `waiting` (quiet 1s, but it may go on alone: the network, a poll with a time limit; a program asleep is `busy`), `busy`, `settled` (quiet; nothing else known), `exited N`, `killed by SIGTERM` (`killed by signal 10` for a signal with no name), or `empty` |
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
| `history_lines` | lines of history the screen shown holds: 0 on the alternate screen, though the main screen's is kept (`main-screen all-history capture` reads it) |
| `copy_mode` | a client has it in copy mode |
| `revision` | grows with every change to the screen |
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
a number or boolean as itself, a string as text, `null` as an answer that
prints `null` (`string` makes it the text).

## `PANE opts capture json`

```json
{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":["…"],PANE FIELDS…}
```

`cursor` is where it is among `lines`: `null` while hidden, or on no line
printed (a blank row trimmed, outside `rows` or `cols`, the screen not
captured). With `marks`, each line holds its marks as text does. With the
`styles` option, `"styles"` holds a list per line of its runs of cells that are not
plain: `{"col":0,"len":3,"fg":1,"bg":"#1e1e20","bold":true,"dim":true,
"italic":true,"underline":true,"blink":true,"inverse":true,
"hidden":true,"strikeout":true}`, each attribute only when set; colours are
palette numbers or `"#rrggbb"`.

`CLIENT capture-client json`:
`{"client":"c1","rows":24,"cols":80,"cursor":[ROW,COL],"lines":["…"]}`.

## What a word made

`new-tab` and `new-workspace` leave the new pane: `json` of a pane, tab or
workspace is its id as a string, `"%4"`, and `X info json` all of it.
`run` and `split` leave an answer (below), which is also the new pane: a
word that wants a pane takes it as one.

## Answers and waits

`json` of an action's answer (`run`, `send-keys`, `send-mouse`, `signal`,
a bare `wait` …): one object, or a line each for several panes (a tab or
workspace waited on, or sent keys). A
pane that settled, still busy at the limit (124), blocked reading its
terminal before the waited text came or went (7), or whose exit was waited for (`wait-exit`,
its own status):

```json
{"pane":"%3","state":"reading","status":0,"output":"$ make test\n12 tests PASS\n"}
{"pane":"%4","state":"exited 5","status":5,"output":"bye\n"}
```

`state` is the pane's (above), or `closed`: a settling action or bare
`wait` whose pane closed answers `{"pane":"%3","state":"closed","status":4}`,
with no `output` and nothing on stderr. `output` is what came since the
input, from the line the keys went in on if it changed (its prompt too;
a `no_change` answer holds nothing), else from the first line that
changed (a lone Enter, a password or raw keys, which echo nothing), or the
screen of a full-screen program (one the keys made leave it: what it
printed after, in whole rows, so old text on the row it starts on too, else that full screen if they changed it, else
the screen), or the screen for status 7; a met `wait-exit` or `wait-gone` answers so too. The state
line's notes are fields, each only when said: `done` (a shell command
ended: its status, `null` if not known), `no_change` (`true`: the keys
were read and drew nothing), `unread` (bytes of input fux holds, not yet
taken by the terminal; what waits in the terminal's own buffers is not
counted). A
wait for a text or a bell answers what met it:

```json
{"pane":"%3","met":"text","text":"PASS","line":"12 tests PASS","status":0}
```

| `met` | Also |
| --- | --- |
| `text` | `text`, `line` |
| `bell` | `notification`: its text, else `null` |

`status` is the exit status. A wait for a text, a bell or the exit that
fails (the program exited first or was started again, 3; the pane closed,
4; the time limit, 124) ends the
program with that status and says why on stderr, `fux: CODE: MESSAGE`: it
leaves no answer to print, but what `run`, `split` or `respawn` started
is printed first, its pane alone (`%2`, as text, `json` or not), and so are the
answers of the words before it. A wait on a tab's or a workspace's panes
that fails in one prints first the answers of those that met it, a line each
as text, `json` or not (`%3 READY`).

## `events-json`, `X events-of-json`

One object a line: `{"event":NAME,"pane":"%3","tab":"@2","workspace":"+2",…}`,
`pane` and `tab` only where they apply. A pane's events come in the order
its journal recorded them, one line each.

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
| `missed` | a pane's events were let go before they were told (past `journal-bytes`) |
| `stopped` | last: `reason`, the server stopped |

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from. CODE stays: `no_server`, `mismatch`,
`no_pane`, `no_tab`, `no_workspace`, `no_target`, `stray_pane`, `exited`,
`still_running`, `not_run`, `own_server`, `people_attached`,
`not_granted`, `name_taken`, `bad_name`, `ambiguous`, `size`, `mouse`,
`no_field`, `id`, `not_reading`, `start`, `paste`, `timeout`, `closed`,
`gone`, `no_buffer`, `not_a_key`, `no_program`,
`no_keys`, `not_a_client`, `grant`, `config`, `unused`, `too_long`; and the
language's: `type` (a word given the wrong kind of value), `stack_underflow`,
`unknown_word`, `open_quote`, `unmatched`, `unclosed`, `range`, `bad_value`,
`overflow`, `div_by_zero`, `call_depth`, and a few more for rarer failures.

## `status json`, `servers json`

```json
{"running":true,"socket":"/run/user/1000/fux/server.sock","version":"0.23.0","protocol":5,
 "server_version":"0.23.0","server_protocol":5,"compatible":true}
[{"name":"test","socket":"/run/user/1000/fux/test.sock","version":"0.23.0","compatible":true}]
```

With no server, `status` gives `running`, `socket`, `version` and `protocol`
alone.
