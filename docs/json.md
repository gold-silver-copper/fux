# What `json` gives

(`fux help-json`.) `json` turns the value on top of the stack into JSON:
what a word left, as below; a string as a JSON string; a number or boolean
as itself; a list as an array of these, each answer in it as `json` gives
it alone (a capture's, a listing's or an action's object; one of several
panes an array of their objects). Rows and columns count from 0; times are milliseconds; ids are strings (`"%3"`,
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
| `version` | its version, `%3:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN`: its pane, its counts and the server's run (`fux help-agents` says what each counts) |
| `program` | what runs in its foreground, as the system names it (Linux keeps a name's first 15 bytes), `null` if its terminal does not say |
| `paused` | a program a restore brought back, waiting for `respawn` |
| `hung_up` | its program closed its terminal and runs on: keys are refused (Linux says so; macOS, where fux holds the terminal open, does not) |
| `exited` | the status its program exited with (a pane whose program is no shell: `run`'s, or a `split` with a program), else `null` |
| `signal` | the signal that killed it (its number), else `null`; both `null` for a program that ended (its version's ENDED is 1) whose status was lost, where `-1` once stood in |
| `idle_ms` | how long since fux last read output from its program (or it started) |
| `alt_screen` | its program holds the full screen |
| `mouse` | its program asked for mouse reports |
| `mouse_mode` | which: `"press"`, `"press_release"`, `"button_motion"`, `"any_motion"`, `"highlight"`, or `null` |
| `bracketed_paste` | its program asked for bracketed paste |
| `pictures` | the pictures it shows (sixel), each at cell `row`, `col`, over `rows` by `cols` cells, the first `cropped` rows gone (scrolled off the top of a screen or region that keeps no history), `width` by `height` pixels |
| `cell` | the size of a cell in pixels its program was last told, `{"width":10,"height":20}`, from a terminal showing it that said one (`CSI 16 t`); `null` while none is known: then it places no pictures and gets no mouse reports in pixels (mode 1016) |
| `cursor_visible` | the cursor shows |
| `history_lines` | lines of history the screen shown holds: 0 on the alternate screen, though the main screen's is kept (`main-screen all-history capture` reads it) |
| `copy_mode` | a client has it in copy mode |
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
{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":["…"],PANE FIELDS…,"above":0}
```

`above` is how many history rows are kept above the first row read:
history not asked for (`null` for `last-output`; rows a `rows` range
leaves out are not counted).

`lines` join a line wrapped at the pane's width whole, unless `wrapped`
(each screen row a line). Blanks at a line's end are trimmed, but with
`styles` those painted (a background, reverse video, underline or
strikeout) stay, in `lines` and `styles` alike: a painted surface shows
whole. `cursor` is `[ROW,COL]` on the screen, from 0, as a mouse event and
`wrapped` count them, whatever
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

`run`, `split`, `new-tab`, `new-workspace` and `respawn` leave an answer
(below), which is also the new pane: a word that wants a pane takes it as
one, and `wait` waits from its version. `json` of a pane, tab or workspace is its id as a string, `"%4"`,
and `X info json` all of it.

## Answers and waits

`json` of an action's answer (`send-keys`, `paste-buffer`, `send-mouse`,
`signal`, `run`, `split`, `new-tab`, `new-workspace`, `respawn`) is
`{"pane":"%3","version":"%3:0.57.0.3.0:RUN","status":0}`: its pane's version from
just before it acted, or as a new pane starts: one object for an action
or wait on a pane, an array for one on a tab or workspace, however many
panes it has or answered.

A `wait` answers each pane whose version differs from the one it waited
from (its program's end too):

```json
{"pane":"%3","version":"%3:0.61.1.4.0:RUN","changed":["screen","bell","done"],"status":0}
{"pane":"%4","version":"%4:0.12.0.0.1:RUN","changed":["screen"],"exited":5,"status":0}
{"pane":"%5","version":"%5:0.9.0.0.1:RUN","changed":[],"signal":15,"status":0}
{"pane":"%2","version":"%2:1.3.0.0.0:RUN","changed":["respawned"],"status":0}
```

`changed` lists `screen`, `bell` and `done` (a count of the version that
grew), or `respawned` (the program started again: its counts start
afresh). `exited` is the program's status if it ended since (`null` if
that status was lost), `signal` the signal that killed it; a wait from a version that already holds its end
waits for something else.

A wait that fails (124 nothing changed, 4 its pane closed) has no JSON: it
ends the program with that status and says why on stderr, one line, `fux:
CODE: MESSAGE`, its answer printed first, as text, after the answers of the
words before it; nothing after it runs, `json` either. Caught by `try`,
its answer is dropped with the rest of the body's values. A wait on a
tab's or a workspace's panes answers a line for each that changed, the
first failing status naming the whole.

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
| `exited` | `status` (`null` if it was lost) or `signal` |
| `done` | `status`: a shell command's |
| `notification` | `text` (the last of a round) |
| `title` | `title` |
| `resized` | `cols`, `rows` |
| `lost` | last: the reader fell behind (status 1) |
| `missed` | a pane's events were let go before they were told (past the 1024 its journal keeps) |
| `stopped` | last: `reason`, the server stopped |

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from. Its exit status is 1, or a
wait's own (above). CODE stays; every one fux gives
(`src/code.rs` says what each means):

- reaching a server: `no_server`, `mismatch`, `client`, `not_granted`,
  `grant`;
- what a command names: `no_pane`, `no_tab`, `no_workspace`, `no_client`,
  `no_buffer`, `no_target`, `stray_pane`, `no_field`, `nothing_there`,
  `selection`, `name_taken`, `id`, `ids_exhausted`, `bad_value`, `usage`;
- what a command meets: `not_running`, `still_running`, `not_run`, `only_shell`,
  `input_full`, `no_marks`, `start`, `size`, `mouse`, `signal`, `log`,
  `not_a_directory`, `not_headless`, `own_server`, `people_attached`;
- how a wait ends: `timeout` (124), `closed` (4), `gone` (4), `lost`;
- a program of words: `steps`, `cancelled`, `not_here`, `not_layout`, `not_config`,
  `config`, `bad_binding`, `check`, `read`, `output`, `nested`,
  `not_a_key`, `no_program`, `no_keys`, `not_a_client`, `paste`;
- the language's own: `open_quote`, `unknown_word`, `missing_token`,
  `bad_name`, `not_top_level`, `unmatched`, `unclosed`, `stack_underflow`,
  `stack_overflow`, `call_depth`, `type`, `div_by_zero`, `overflow`,
  `unset`, `unused`, `range`, `fail`, `too_long`, `bad_codes`, `contract`.

A program catches the failures its words give, by code, with `try` (`fux
help try`); a code no running word gives is refused before it runs.

## `status json`, `servers json`

```json
{"socket":"/run/user/1000/fux/server.sock","version":"0.35.0","protocol":16,
 "server_version":"0.35.0","server_protocol":16,"compatible":true}
[{"name":"test","socket":"/run/user/1000/fux/test.sock","version":"0.35.0","compatible":true}]
```

With no server, `status` fails (`no_server`), saying the socket and whether
a server that stopped left it there.
