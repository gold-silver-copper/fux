# What `json` gives

(`fux help json`.) `json` turns the value on top of the stack into JSON:
what a word left, as below; a string as a JSON string; a number or boolean
as itself; a list as an array of these, each answer in it as `json` gives
it alone (a capture's, a listing's or an action's object; one of several
panes an array of their objects). Rows and columns count from 0; times are milliseconds; ids are strings (`"%3"`,
`"@2"`, `"+1"`, `"c1"`).

## A pane

`ls json` gives each pane these; `PANE capture json` gives them too, but
for `id`, `name`, `label` and `pid` (it says `pane`):

| Field | Value |
| --- | --- |
| `id` | `"%3"` |
| `name` | its given name (`run ... name` or `rename`), else `null` |
| `label` | display text: its given name, else title, launch program basename, or id; never a lookup name |
| `title` | what the program set (OSC 0 or 2), else `""`: its first 256 characters, without control characters |
| `rows`, `cols` | its size |
| `pid` | its program's process, `null` once it exited, and while a respawn of it is under way |
| `version` | its version, `%3:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN`: its pane, its counts and the server's run (`fux help agents` says what each counts) |
| `program` | what runs in its foreground, as the system names it (Linux keeps a name's first 15 bytes), `null` if its terminal does not say |
| `hung_up` | its program closed its terminal and runs on: keys are refused (Linux says so; macOS, where fux holds the terminal open, does not) |
| `exited` | the status its program exited with (a pane whose program is no shell: `run`'s, or a `split` with a program), else `null` |
| `signal` | the signal that killed it (its number), else `null`; both `null` for a program that ended (its version's ENDED is 1) whose status was lost |
| `idle_ms` | how long since fux last read output from its program (or it started) |
| `alt_screen` | its program holds the full screen |
| `mouse` | its program asked for mouse reports |
| `mouse_mode` | which: `"press"`, `"press_release"`, `"button_motion"`, `"any_motion"`, `"highlight"`, or `null` |
| `bracketed_paste` | its program asked for bracketed paste |
| `pictures` | the pictures it shows (sixel), oldest first, a newer one covering the pixels of an older one that its own lie on, each at cell `row`, `col`, over `rows` by `cols` cells, the first `cropped` rows gone (scrolled off the top of a screen or region that keeps no history), `width` by `height` pixels |
| `lost` | what fux let go of its program's since it started, each kind counted (`{"input":0,"replies":1,"copies_cut":0,"copies_not_text":0,"pictures":0,"memory":0}`): `input`, pieces of input (keys, a paste, `send-keys`, `send-mouse`, the mouse and focus reports a client sends it) its full input queue refused, which takes nothing then until its program reads; `replies`, replies to its queries (a cursor report, a colour, its size) and in-band resize reports (mode 2048) dropped: those past 4 KiB of replies to one read of its output, all of one read's while its input queue was full, or a resize report the full queue refused (a colour-scheme report it is sent unasked (mode 2031) and refused is sent again, not lost); `copies_cut` and `copies_not_text`, its copies (OSC 52) that copied nothing, longer than the 64 KiB a string may be, or not base64 (RFC 4648, padded or not) of UTF-8 text (none is counted with `str clipboard false set`, which copies nothing; a copy made but refused for a client too far behind is that client's loss, told to it, not counted here); `pictures`, sixel pictures it drew that were not kept whole, each counted as it ends: begun while its host drew no sixel (its tab not shown, or its host draws no sixel or said no cell size), ended once it did not, lifted past a scroll region's top as it was placed, or cut: what it drew past what a picture may keep (4,096 pixels a side, 8,847,360 pixels in all, and the pictures' memory budget) was not kept, placed or not. One that drew nothing, or that CAN ended, is none; one placed whole and let go later, the oldest first as newer ones need the room, is not counted, as history let go is not; `memory`, times reading its output, or a resize, was refused memory (fux-vt's limits, `fux-vt/THREATS.md` 1.3), each losing only what it was for: history a resize let go counts too, and a size refused counts once until another size is asked or the pane keeps its own. A client showing it is told each kind, with how many, once a run of losses of that kind, which a success of the kind ends: a run of input ends as its program reads; of replies, as its program reads after its queue refused, or at a read that drops none while its queue takes input; of copies, at a copy made; of pictures, at one placed whole; of memory, at a read of its output or a resize that refused none (a read: a frame and what follows it, or output once all of it is read); a program's end or a respawn starts every run anew. A client typing into it is also told each key refused, as it types |
| `cell` | the size of a cell in pixels its program was last told, `{"width":9,"height":18}`: its tab's host's (`CSI 16 t`, asked again each time that terminal is resized, as a font zoom resizes it); `null` while it is told none (no terminal shows its tab, or its host said none): then it places no pictures |
| `cursor_visible` | the cursor shows |
| `history_lines` | lines of history the screen shown holds: 0 on the alternate screen, though the main screen's is kept (`main-screen all-history capture` reads it) |
| `copy_mode` | a client has it in copy mode |
| `command_running` | a shell command runs (OSC 133 marks) |
| `last_status` | the last shell command's status, else `null` |
| `bells` | bells and notifications rung since it started |
| `notification` | the last notification's text, else `null`: its first 500 characters, without control characters |
| `truncated` | which of `"title"`, `"notification"` and `"output"`, in that order, were cut, else `[]`: the fields above that the program sent more of than they keep, the rest dropped (`["notification"]`, `["title","notification"]`); `"output"` once its program left more than 1 MiB on its terminal as it ended, the rest not read (till it is respawned) |
| `input_unread` | bytes of input fux queued for its program and had not yet written to its terminal as it ended, let go (till it is respawned), else 0. Input written to its terminal that the program never read is not counted: the system does not say |
| `cwd`, `argv` | the foreground process's directory and words, where the system says; JSON's text being UTF-8, a byte that is not is shown as U+FFFD |

## `ls json`

```json
{"workspaces":[{"id":"+1","name":null,"label":"+1","tabs":[{"id":"@1","name":null,"label":"%1","panes":[PANE…]}]}],
 "clients":[{"id":"c1","rows":24,"cols":80,"workspace":"+1","tab":"@1","pane":"%1","zoom":false}],
 "events":"42:48211-1867a3f9c2b8e7d6"}
```

`events` is a cursor, `SEQ:RUN`: the number the session's journal of events
gives next, and the server's run, read with the listing. A stream from it
(`opts str CURSOR from events-of`) tells every event (the table under
`events`) after the listing, none before it, or with `missed` how many of
those the journal let go first: then read `ls` again. `ls` ends with it
too, as `events CURSOR`.

`X info json` gives that pane, tab (`{"id","name","label","panes"}`) or workspace
(`{"id","name","label","tabs"}`) alone; `X str FIELD get` leaves one of its fields:
a number or boolean as itself, a string as text, `null` as an answer that
prints `null` (`string` makes it the text). A tab or workspace has an optional
given `name` (`null` when absent) and a derived display `label`. A tab's
label is its given name or its first pane's label; a workspace's is its
given name or id. `get` reads only the object's own fields.

## `PANE capture json`

```json
{"pane":"%1","screen_rows":23,"screen_cols":80,"screen_cursor":[ROW,COL],"lines":["…"],"wrap_pending":false,PANE FIELDS…,"above":0}
```

`above` is how many history rows are kept above the first row read:
history not asked for (`null` for `last-output`; rows a `rows` range
leaves out are not counted).

`lines` join a line wrapped at the pane's width whole, unless `wrapped`
(each screen row a line). Blanks at a line's end are trimmed, but with
`styles` those painted (a background, reverse video, underline or
strikeout) stay, in `lines` and `styles` alike: a painted surface shows
whole. `screen_rows` and `screen_cols` are the screen's size;
`screen_cursor` is `[ROW,COL]` on the screen, from 0, as a mouse event and
`wrapped` count them, whatever
`lines` join or `rows` and `cols` take: `null` while hidden or on the
screen not captured (`main-screen` under a full-screen program). `wrap_pending`:
a glyph went into the last column, autowrap on, and the next goes on the
next line, though the cursor shows on that last cell. With `marks`, each line holds its marks as text does. With the
`styles` option, `"styles"` holds a list per line of its runs of cells that are not
plain: `{"col":0,"len":3,"fg":1,"bg":"#1e1e20","bold":true,"inverse":true,
"hidden":true,"dim":true,"italic":true,"strikeout":true,"underline":true,
"blink":true}`, each attribute only when set; colours are
palette numbers or `"#rrggbb"`.

`CLIENT capture-client json`:
`{"client":"c1","screen_rows":24,"screen_cols":80,"screen_cursor":[ROW,COL],"lines":["…"]}`.

## What a word made

`run`, `split`, `new-tab`, `new-workspace` and `respawn` leave an answer
(below), which is also the new pane: a word that wants a pane takes it as
one, and `wait` waits from its version. `json` of a pane, tab or workspace is its id as a string, `"%4"`,
and `X info json` all of it. `X panes json` and `X tabs json` are arrays
of them in layout order, `["%3","%4"]`.

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
grew), or `respawned` (the program started again: its counts but
`RESPAWNS` start afresh). `exited` is the program's status if it ended
since (`null` if that status was lost), `signal` the signal that killed
it; a wait from a version that already holds its end waits for
something else.

A wait that fails (124 nothing changed, 4 its pane closed) answers too,
with its status: `{"pane":"%3","version":"%3:0.57.0.3.0:RUN","status":124}`,
the version it is still at; `{"pane":"%3","status":4}`, its pane closed.
Uncaught, it ends the program with that status and says why on stderr, one
line, `fux: CODE: MESSAGE`, its answer printed first, as text, after the
answers of the words before it; nothing after it runs. Caught by `try`,
the handler gets its answer in place of the message, under the code:
`str timeout try %3 5 timeout wait catch drop end json` prints it, and
`wait` on it waits again from the version it is still at. A wait on a
tab's or a workspace's panes answers each that changed, the first failing
status naming the whole.

## `events`, `X events-of`

One object a line: `{"event":NAME,"pane":"%3","tab":"@2","workspace":"+2",…}`,
`pane`, `tab` and `workspace` each only where it applies: none of them
on `ready`, `missed`, `gone` or `stopped`. Events come in the
order they happened in the whole session, one line each: each is written
to the session's journal where it happens, and a stream reads the journal from
its own place. A reader that falls behind is told the rest as it reads.
A stream starts from now, or with `from` from a cursor, `SEQ:RUN`, as
`ls` and `ready` give it (`opts str CURSOR from events-of` for everything,
`X str CURSOR from events-of` for X, which must still be there). A cursor
of another server run, or past the journal's end, is refused
(`bad_value`).
The journal keeps the session's last 16,384 entries, for every stream
alike: a pane that makes many in one round (a flood of bells) can make
even a stream that reads at once, of another pane, miss some, which
`missed` says.

| `event` | Also |
| --- | --- |
| `ready` | first: `from`, the cursor it reads from; from there on nothing is missed unsaid |
| `gone` | last: everything `events-of` named has closed (status 4) |
| `created` | `name`, for a tab or workspace, `null` when unnamed |
| `renamed` | `name`: a pane's, tab's or workspace's new name |
| `closed`, `bell` | |
| `respawned` | at every respawn. Its fields, only when the program it ran over had not ended (a `kill-first` respawn), as one that ended said them with its `exited`: `truncated` (`true`) if it left more than 1 MiB on its terminal: the rest was not read; `input_unread`, if any: bytes of input fux queued for it and had not yet written to its terminal, let go |
| `moved` | `from_tab`, `from_workspace`: where the pane was; `events-of` either place tells it |
| `exited` | `exited`, its status (`null` if it was lost), or `signal`: its program's end, as a pane's JSON and a wait say it; `truncated` (`true`) if it left more than 1 MiB on its terminal as it ended: the rest was not read; `input_unread`, if any: bytes of input fux queued for it and had not yet written to its terminal, let go (as a pane's `input_unread`); a shell's pane then closes (`closed`) |
| `done` | `status`: a shell command's, `null` if the shell gave none |
| `notification` | `text`, `truncated` (`true`) if cut as a pane's `notification` is |
| `title` | `title`, `truncated` (`true`) if cut as a pane's `title` is |
| `resized` | `cols`, `rows` |
| `missed` | `count`: entries the journal let go (past what it keeps) before this stream was told them, or, from a cursor, entries after it let go before the stream began; with `events-of`, some may be of other things |
| `stopped` | last: `reason`, the server stopped |

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from. Its exit status is 1, or a
wait's own (above). CODE stays; every one fux gives
(`src/code.rs` says what each means):

- reaching a server: `no_server`, `mismatch`, `client`;
- what a command names: `no_pane`, `no_tab`, `no_workspace`, `no_client`,
  `no_buffer`, `no_target`, `stray_pane`, `no_field`, `nothing_there`,
  `selection`, `name_taken`, `ids_exhausted`, `bad_value`, `usage`;
- what a command meets: `not_running`, `still_running`, `not_run`, `only_shell`,
  `input_full`, `stale`, `no_marks`, `start`, `size`, `mouse`, `signal`,
  `not_headless`, `own_server`, `people_attached`;
- how a wait ends: `timeout` (124), `closed` (4), `gone` (4);
- a program of words: `steps`, `cancelled`, `not_here`, `not_config`,
  `config`, `bad_binding`, `check`, `read`, `unset_var`, `output`, `nested`,
  `not_a_key`, `no_program`, `no_keys`, `not_a_client`;
- the language's own: `open_quote`, `unknown_word`, `missing_token`,
  `bad_name`, `not_top_level`, `unmatched`, `unclosed`, `stack_underflow`,
  `stack_overflow`, `call_depth`, `type`, `div_by_zero`, `overflow`,
  `unset`, `unused`, `range`, `fail`, `too_long`, `bad_codes`, `contract`.

A program catches the failures its words give, by code, with `try` (`fux
help try`); a code no running word gives is refused before it runs. Of the
language's own, fux's words give `unused`, `unknown_word` and `bad_name` as
they run, so a `try` may name them too.

## `status json`

```json
{"socket":"/run/user/1000/fux/server.sock","version":"0.36.0","protocol":24,
 "server_version":"0.36.0","server_protocol":24,"compatible":true}
```

With no server, `status` fails (`no_server`), saying the socket and whether
a server that stopped left it there. Every server in fux's directory:
`status json` on each socket there (`fux help servers`).
