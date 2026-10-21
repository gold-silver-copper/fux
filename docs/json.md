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

`ls json` gives each pane these; `PANE capture json` gives them too, but
for `id`, `name`, `title` and `pid` (it says `pane`):

| Field | Value |
| --- | --- |
| `id` | `"%3"` |
| `name` | its name: the program's, or what `rename` gave it |
| `title` | what the program set (OSC 0 or 2), else `""` |
| `rows`, `cols` | its size |
| `pid` | its program's process, `null` once it exited |
| `program` | what runs in its foreground, `null` if unknown |
| `state` | what it is doing: `reading` (blocked on its terminal, every key read), `waiting` (quiet 1s, but it may go on alone: the network, a poll with a time limit; a program asleep is `busy`), `busy`, `settled` (quiet; nothing else known), `exited N`, `killed by SIGTERM` (`killed by signal N` for a signal fux has no name for), or `empty`; for a relay's remote program (`koh connect`), as the relay's last state report says (`OSC 133 ; S ; activity=STATE`), while no mark came after it and it came after the last input, the screen quiet a moment after it |
| `paused` | a program a restore brought back, waiting for `respawn` |
| `exited` | the status its program exited with (a pane `run` started), else `null` |
| `signal` | the signal that killed it (its number), else `null` |
| `idle_ms` | how long its screen has shown no change |
| `alt_screen` | its program holds the full screen |
| `mouse` | its program asked for mouse reports |
| `mouse_mode` | which: `"press"`, `"press_release"`, `"button_motion"`, `"any_motion"`, `"other"` (a mode fux does not name), or `null` |
| `bracketed_paste` | its program asked for bracketed paste |
| `pictures` | the pictures it shows (sixel), each at cell `row`, `col`, over `rows` by `cols` cells, the first `cropped` rows gone (scrolled off the top of a screen or region that keeps no history), `width` by `height` pixels |
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

## `PANE capture json`

```json
{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":["…"],PANE FIELDS…}
```

`lines` join a line wrapped at the pane's width whole, unless `wrapped`
(each screen row a line). Blanks at a line's end are trimmed, but with
`styles` those painted (a background, reverse video, underline or
strikeout) stay, in `lines` and `styles` alike: a painted surface shows
whole. `cursor` is `[ROW,COL]` on the screen, from 0, as a mouse event, a
state line's `· cursor ROW,COL` and `wrapped` count them, whatever
`lines` join or `rows` and `cols` take: `null` while hidden or on the
screen not captured (`main-screen` under a full-screen program). `wrap_pending` (after `lines`):
a glyph went into the last column, autowrap on, and the next goes on the
next line, though the cursor shows on that last cell. With `marks`, each line holds its marks as text does. With the
`styles` option, `"styles"` holds a list per line of its runs of cells that are not
plain: `{"col":0,"len":3,"fg":1,"bg":"#1e1e20","bold":true,"inverse":true,
"hidden":true,"dim":true,"italic":true,"strikeout":true,"underline":true,
"blink":true}`, each attribute only when set; colours are
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
0, its own status in `state`):

```json
{"pane":"%3","state":"reading","status":0,"output":"$ make test\n12 tests PASS\n"}
{"pane":"%4","state":"exited 5","status":0,"output":"bye\n"}
```

`state` is the pane's (above), or `closed`: a settling action or bare
`wait` whose pane closed answers
`{"pane":"%3","state":"closed","status":4,"code":"closed"}`, with no
`output` and nothing on stderr. `code` names a status that is not 0. `output` is what came since the
input, from the line the keys went in on if it changed (its prompt too;
a `no_change` answer holds nothing), else from the first line that
changed (a lone Enter, a password or raw keys, which echo nothing), or the
screen of a full-screen program (one the keys made leave it: what it
printed after, in whole rows, so old text on the row it starts on too, else that full screen if they changed it, else
the screen), or the screen for status 7; a met `wait-exit` or `wait-gone` answers so too. The state
line's notes are fields, each only when said: `done` (a shell command
ended: its status, `null` if not known), `marked` (`false`: a line typed
that a shell in front reads again after, where nothing marked a
command, so no `done` or status comes; a relay's program counts as a
shell only if the relay's words name one, `-- bash -i`), `no_change` (`true`: the keys
were read and drew nothing), `command_running` (`true`: a marked shell
command runs on, one the keys went into or one that took the full
screen; never once the pane's program exited), `redraws` (`true`: a full-screen program that redraws by
itself read the keys and drew after them; what it draws later may
differ), `wait_with` (`"wait-exit"` or `"wait-done"`: a pane `waiting`
that is no full-screen program, maybe at work, blocked off its
terminal, or a relay that said nothing since the input, and the wait word
that waits for its end; with `remote`, `"wait-exit"` or `"wait-text"`),
`remote` (the name of a remote client in front, `"ssh"`, that reports
nothing of what it runs, in every answer, as its text says: its state is the client's, not that program's;
`wait_with` says how to wait for it: `"wait-exit"` for a command run on
a pipe there, which the client ends with, `"wait-text"` for a terminal
there), `cursor_rows` (with `cursor`, on a line wrapped at the pane's width,
which the answer prints joined: `{"from":ROW,"text":["…","…"]}`, the
screen rows the line takes, four at most), `cursor` (`[ROW,COL]` of the screen
after keys or a mouse event: after a line edited where a program reads
it, when text shows under or right of the cursor on its row, or when the
input moved the cursor alone; never while it is hidden or its program
exited), `highlight` (after keys
or a mouse event: up to three runs they highlighted anew, each
`{"row","col","len","text"}`, row and column from 0 as a mouse event counts
them), `unread` (bytes of input fux holds, not yet
taken by the terminal; what waits in the terminal's own buffers is not
counted), `lost` (the program exited: the keys and texts it never read,
which went nowhere, `["C-q","\"ab\""]`; through a relay that said its
program ended having read what came before, what came after), `unanswered`
(a relay ended without saying so: what it sent on after the first key or
line its program did not say it read, which likely went nowhere). A
wait for a text or a bell answers what met it too:

```json
{"pane":"%3","state":"busy","met":"text","text":"PASS","line":"12 tests PASS","status":0}
```

| `met` | Also |
| --- | --- |
| `text` | `text`, `line` |
| `bell` | `notification`: its text, else `null` |

`status` is the exit status: an answer left at the end (alone, as its
JSON or in a list; the first of several) gives the run its status, one
dropped or taken by a word gives none. A wait for a text, a bell or the exit that
fails (the program exited first or was started again, 3; the pane closed,
4; the shell command `wait-done` waited on ended first, 5; the time limit, 124) ends the
program with that status and says why on stderr, one line, `fux: CODE:
MESSAGE`; its answer is printed first, as text, with its pane's last
lines (three when its program exited, one at the time limit; none for a
pane closed or started again). Nothing after the wait runs, `json` either.
The answers of the words before it are printed first too. A wait on a
tab's or a workspace's panes answers a line each, the first failing status
naming the whole.

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
| `missed` | a pane's events were let go before they were told (past `journal-bytes` or `journal-total`) |
| `stopped` | last: `reason`, the server stopped |

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from. Its exit status is its code's: 1,
`usage`'s 2, a wait's its own (below). CODE stays; every one fux gives
(`src/code.rs` says what each means):

- reaching a server: `no_server`, `mismatch`, `client`, `not_granted`,
  `grant`;
- what a command names: `no_pane`, `no_tab`, `no_workspace`, `no_client`,
  `no_buffer`, `no_target`, `stray_pane`, `no_field`, `nothing_there`,
  `selection`, `name_taken`, `id`, `ids_exhausted`, `bad_value`, `usage`;
- what a command meets: `not_running`, `still_running`, `not_run`, `only_shell`,
  `not_reading`, `start`, `size`, `mouse`, `signal`, `log`,
  `not_a_directory`, `not_headless`, `own_server`, `people_attached`;
- how a wait ends: `exited` (3), `ended` (5), `timeout` (124), `closed` (4), `gone` (4),
  `lost`, `stopped` (7);
- a program of words: `steps`, `cancelled`, `not_here`, `not_layout`, `not_config`,
  `config`, `bad_binding`, `check`, `read`, `output`, `nested`, `unused`,
  `not_a_key`, `no_program`, `no_keys`, `not_a_client`, `paste`;
- the language's own: `open_quote`, `unknown_word`, `missing_token`,
  `bad_name`, `not_top_level`, `unmatched`, `unclosed`, `stack_underflow`,
  `stack_overflow`, `call_depth`, `type`, `div_by_zero`, `overflow`,
  `unset`, `range`, `fail`, `too_long`, `contract`.

## `status json`, `servers json`

```json
{"running":true,"socket":"/run/user/1000/fux/server.sock","version":"0.33.0","protocol":14,
 "server_version":"0.33.0","server_protocol":14,"compatible":true}
[{"name":"test","socket":"/run/user/1000/fux/test.sock","version":"0.33.0","compatible":true}]
```

With no server, `status` gives `running`, `socket`, `version` and `protocol`
alone, and `"left":true` when its socket is left from a server that
stopped.
