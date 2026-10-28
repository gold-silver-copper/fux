# What `json` gives

(`fux help json`.) `json` turns the value on top of the stack into JSON.
Small results are ordinary strings. Beyond 1,000,000 characters it returns shared text of kind
`rendered`: `print` and implicit command output emit it under the existing
128 MiB finite-observation grant. Explicit `string` conversion still obeys
the language string limit. All live imported storage shares one 128 MiB grant: captures, rendered output,
records, strings, lists and options. Reservations follow shared allocations;
clones and projected children keep their reservation without double charging.
Dropping the last reference refunds its bytes, so polling and discarding results
can continue. Per-value limits and traversal charges still apply.

The JSON represents what a word left, as below; a string as a JSON string; a number or boolean
as itself; a list as an array of these, each answer in it as `json` gives
it alone (a capture's, a listing's or an action's object; one of several
panes an array of their objects). Rows and columns count from 0; times are milliseconds; ids are strings (`"%3"`,
`"@2"`, `"+1"`, `"c1"`).

## A pane

`ls json` gives each pane its identity and size, with process fields under `state`; `PANE capture json` gives them too, but
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
| `foreground_group` | the terminal's observed foreground process group id, or `null` if unknown; its leader may have exited while other members still run |
| `foreground_leader` | the process id of that group's leader when observed to exist, else `null`; fux does not search for another group member |
| `program` | the observed foreground leader's program name (Linux keeps its first 15 bytes), or `null` if unknown |
| `launch_argv` | the checked arguments of its saved launch, for both shell and program roles, retained separately from live observations |
| `hung_up` | its program closed its terminal and runs on: keys are refused (Linux says so; macOS, where fux holds the terminal open, does not) |
| `read_error` | an unexpected PTY read failure as errno text, or `null`: a failed reader is no longer polled and refuses keys; the cause stays after exit until respawn |
| `exited` | the status its program exited with (a pane whose program is no shell: `run`'s, or a `split` with a program), else `null` |
| `signal` | the signal that killed it (its number), else `null`; both `null` for a program that ended (its version's ENDED is 1) whose status was lost |
| `alt_screen` | its program holds the full screen |
| `mouse` | its program asked for mouse reports |
| `mouse_mode` | which: `"press"`, `"press_release"`, `"button_motion"`, `"any_motion"`, `"highlight"`, or `null` |
| `bracketed_paste` | its program asked for bracketed paste |
| `pictures` | the pictures it shows (sixel), oldest first, a newer one covering the pixels of an older one that its own lie on, each at cell `row`, `col`, over `rows` by `cols` cells, the first `cropped` rows gone (scrolled off the top of a screen or region that keeps no history), `width` by `height` pixels |
| `lost` | what fux let go of its program's since it started, each kind counted (`{"input":0,"replies":1,"copies_cut":0,"copies_not_text":0,"pictures":0,"memory":0}`): `input`, pieces of input (keys, a paste, `send-keys`, `send-mouse`, the mouse and focus reports a client sends it) its full input queue refused, which takes nothing then until its program reads; `replies`, replies to its queries (a cursor report, a colour, its size) and in-band resize reports (mode 2048) dropped: those past 4 KiB of replies to one read of its output, all of one read's while its input queue was full, or a resize report the full queue refused (a colour-scheme report it is sent unasked (mode 2031) and refused is sent again, not lost); `copies_cut` and `copies_not_text`, its copies (OSC 52) that copied nothing, longer than the 64 KiB a string may be, or not base64 (RFC 4648, padded or not) of UTF-8 text (none is counted for a pane created with `false program-clipboard`, which copies nothing; a copy made but refused for a client too far behind is that client's loss, told to it, not counted here); `pictures`, sixel pictures it drew that were not kept whole, each counted as it ends: begun while its host drew no sixel (its tab not shown, or its host draws no sixel or said no cell size), ended once it did not, lifted past a scroll region's top as it was placed, or cut: what it drew past what a picture may keep (4,096 pixels a side, 8,847,360 pixels in all, and the pictures' memory budget) was not kept, placed or not. One that drew nothing, or that CAN ended, is none; one placed whole and let go later, the oldest first as newer ones need the room, is not counted, as history let go is not; `memory`, times reading its output, or a resize, was refused memory (fux-vt's limits, `fux-vt/THREATS.md` 1.3), each losing only what it was for: history a resize let go counts too, and a size refused counts once until another size is asked or the pane keeps its own. A client showing it is told each kind, with how many, once a run of losses of that kind, which a success of the kind ends: a run of input ends as its program reads; of replies, as its program reads after its queue refused, or at a read that drops none while its queue takes input; of copies, at a copy made; of pictures, at one placed whole; of memory, at a read of its output or a resize that refused none (a read: a frame and what follows it, or output once all of it is read); a program's end or a respawn starts every run anew. A client typing into it is also told each key refused, as it types |
| `cell` | the size of a cell in pixels its program was last told, `{"width":9,"height":18}`: its tab's host's (`CSI 16 t`, asked again each time that terminal is resized, as a font zoom resizes it); `null` while it is told none (no terminal shows its tab, or its host said none): then it places no pictures |
| `cursor_visible` | the cursor shows |
| `history_bytes` | the pane's current history budget, retained across respawn |
| `program_clipboard` | whether this pane accepts program OSC 52 copies; fixed at creation |
| `reflow` | whether primary-screen lines rewrap on resize; retained across reset and respawn |
| `alternate_scroll_lines` | the ordinary wheel step when the program enables alternate scroll; 0–32767 arrows |
| `picture_bytes` | the creation picture budget, 0–96 MiB; zero disables picture support |
| `history_lines` | lines of history the screen shown holds: 0 on the alternate screen, though the main screen's is kept (`main-screen all-history capture` reads it) |
| `copy_mode` | a client has it in copy mode |
| `command_running` | a shell command runs (OSC 133 marks) |
| `last_status` | the last shell command's status, else `null` |
| `bells` | bells and notifications rung since it started |
| `notification` | the last notification's text, else `null`: its first 500 characters, without control characters |
| `truncated` | which of `"title"`, `"notification"` and `"output"`, in that order, were cut, else `[]`: the fields above that the program sent more of than they keep, the rest dropped (`["notification"]`, `["title","notification"]`); `"output"` once its program left more than 1 MiB on its terminal as it ended, the rest not read (till it is respawned) |
| `input_unread` | bytes of input fux queued for its program and had not yet written to its terminal as it ended, let go (till it is respawned), else 0. Input written to its terminal that the program never read is not counted: the system does not say |
| `cwd`, `argv` | the observed foreground leader's directory and arguments, each `null` where unknown; separate OS reads may race with a process changing or exiting. No launch arguments stand in for unknown live arguments. JSON's text being UTF-8, a byte that is not is shown as U+FFFD |

## Snapshots and local records

`SCOPE str PROJECTION snapshot` returns one immutable record. `SCOPE` is
`all`, a pane, tab, workspace or client. `PROJECTION` is `facts`, `topology`
or `recipe`. `ls` takes a facts snapshot of all scopes; `X info` takes one
of X. Plain output lists the hierarchy; `json` renders the same observation.

Every scope has the same envelope: `run`, `events`, `scope`, `projection`,
`workspaces`, `tabs`, `panes`, and `clients`. The four collections are flat,
in hierarchy order. Workspace `tabs` is an ordered list of tab handles;
a tab's `workspace` and a pane's `tab` identify their parents. IDs in fuxth
are actual handles carrying this server run. JSON renders them as strings.
An ordinary string that looks like `%3` remains a string.

```fux
all str topology snapshot let held
held str panes get each str id get print end
held str clients get each str pane get print end
```

`get` on a record, held action answer, or capture is local, even if the
source pane has changed, respawned, or closed. A held answer's `pane`,
`version`, `status`, and applicable change/exit fields come from that answer.
A capture exposes its saved `pane` (or `client`), version, screen geometry,
lines, optional styles, and captured state fields such as `last_status`.
An absent field still fails `no_field`; it never falls back to a live read.
Use `ANSWER pane str name get` to request the live pane name explicitly.
Arrays become fuxth lists, objects become records, booleans and integers keep
their types, and strings keep their exact text. Null has kind `null`; it is
neither false nor an empty string/list. An absent field fails `no_field`.
Unsigned numbers above i64 fail `range`. There is no float or string-to-ID
conversion. `buffers` now returns a list of records; `buffers json` renders
its array. `X str FIELD get` on a live handle and `panes`/`tabs` remain native
selective reads. `get id` and container membership return typed handles.

Each workspace has `id`, `name`, `label`, and `tabs`. Its `tabs` is always the
full ordered membership, including in a tab- or pane-scoped snapshot, whose
`tabs` facts collection contains only the selected tab. Each tab fact has
`id`, `workspace`, `name`, `label`, `fit` (`fixed` or `clients`), `cols`,
`rows`, `nodes`, `layout_revision`, and `host`. Client fit reports its retained size.

`layout_revision` is `TAB:REVISION:RUN`, the native layout revision, or null if its
counter is exhausted. Screen output does not change it. `nodes` uses the
shared flat layout schema: the root is index zero, a leaf is a typed pane
handle, and a split is `[AXIS, FIRST_SHARE, SECOND_SHARE, FIRST, SECOND]`.
The axis is `horizontal` or `vertical`; shares are positive integers and
children are indices in that same list. These values feed `arrange` directly. Every node is reachable once, with
one leaf per pane. Layout depth does not consume record nesting. A successful `arrange` emits
one `arranged` event with the tab and workspace.

Each pane has `id`, `tab`, `name`, `label`, `title`, `pid`, `rows`, `cols`,
and `version`. A name is given or null; a label is display text. Facts adds
`state`, containing the process/terminal fields above. Topology uses only
retained state and performs no process metadata queries. Recipe adds `launch`:
`choice` (`shell` or `program`), ordered `argv`, `env`, `initial_cwd`,
`observed_cwd`, `history_bytes`, `program_clipboard`, `reflow`, `alternate_scroll_lines`, and
`picture_bytes`. Both launch choices
retain their original inputs. Environment entries have `name` and `value`;
null means removal. OS text uses `{encoding: utf8, text: ...}` or
`{encoding: hex, text: ...}` for exact non-UTF-8 bytes. Observed cwd is null
when unknown, `{status: known, path: ...}`, or
`{status: failed, operation: cwd, errno: N}`. Replay launches new processes;
these are launch inputs, not saved process memory.

Each client has `id`, `kind` (`terminal` or `headless`), `read_only`, physical
`rows`/`cols`, `pane_rows`/`pane_cols`, optional `workspace`/`tab`/`pane`,
`zoom`, `opening` (null without an open screen), and `mode`.

All includes every root and view. Workspace includes its descendants and
views showing it; tab includes its whole tab and parent workspace reference.
Pane facts includes only that pane's facts, with its tab topology and relevant
views; pane topology/recipe includes its whole tab. Client includes that
client and its current workspace closure. Missing explicit scopes fail.

`events` is the journal cursor `SEQ:RUN`, gathered with the hierarchy in one
server turn. A stream from it (`opts str CURSOR from events-of`) starts after
that observation; a missed journal prefix requires another snapshot. OS
observations happen during the turn but are not simultaneous OS snapshots.
A pane version does not certify process metadata or client focus.

Snapshots allow at most 8 MiB of native construction/decoded allocation and
65,536 entity, workspace-membership and tree nodes. Decode checks the same
inventory as construction; raw OS bytes, argv items and JSON members consume
the byte/allocation grant rather than one entity node per byte or item. Client projection separately admits at most
8 MiB and 65,536 values/fields, with record nesting at most 32. It retains
the native schema for plain rendering and shares immutable records on clone.
The tighter language limits still apply, including 1,000,000 characters per
string and 16,000,000 expanded elements per list/run. Opaque values charge
expanded traversal; imports also share the live-storage grant above. Constructed strings
and lists charge the run’s construction grant; repeated shared values count again. A too-large observation fails `too_long`, never truncates or
combines multiple turns. Narrower scopes are separate observations.

A tab's `host` is the terminal client supplying its panes' capability answers,
or `null`. Of terminals showing it, the last typist wins, else the first
attached. Headless clients never host. No capabilities are combined across
clients; an unanswered query stays unknown even if a different client answered.

Each client's `terminal` preserves evidence; `output` describes what fux will
attempt on that client, not what the terminal acknowledged or displayed.
Headless clients have no terminal reports.

| Field | Value |
| --- | --- |
| `terminal.sixel_reported` | `true` or `false` from DA1; `null` before any reply |
| `terminal.cell_reported` | last reported `{"width":10,"height":20}` in pixels, even zero or a size refused for graphics; `null` before any reply |
| `terminal.underline_styles_reported` | `true` after positive evidence; otherwise `null`, since the decoder supplies no negative report |
| `terminal.kitty_flags_reported` | last decoded keyboard flags, including 0; `null` before any reply |
| `output.graphics` | `"text"` without an accepted cell size; `"cell"` with a cell but no positive sixel report; `"sixel"` with both. A picture still requires the pane host's support and the same cell size on the displaying client |
| `output.underline_styles` | whether styled underlines are emitted; false emits plain underlines |
| `output.clipboard` | `"osc52"` if this client's `terminal-clipboard` permits output, else `null`; not a delivery acknowledgement |
| `output.kitty_flags_requested` | 5 once fux has requested disambiguate and alternate keys, else `null`; distinct from the last flags reported |

Cell reports are asked again on resize; the previous report stays until
another arrives. These fields and `host` are in the same bounded listing,
not a separate capability query. They can change without a pane version
changing; the journal does not promise a complete stream of terminal replies.

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

`rows` and `cols` select half-open ranges, `start..end`, just like `slice`.
Equal endpoints select nothing; ranges past available rows or columns are
clipped to the available data. Negative, reversed, or endpoints past 65535
fail with `range`. Rows count from the top of the requested
screen and history; joined captures expand nonempty row ranges to whole
logical lines. `wrapped` keeps only the physical rows selected. Cutting a
wide glyph at either column boundary leaves a blank in the selected half.

`CLIENT capture json`:
`{"client":"c1","screen_rows":24,"screen_cols":80,"screen_cursor":[ROW,COL],"lines":["…"]}`.

Finite observations have hard limits: 32 MiB of wire bytes and a 128 MiB
allocation grant for construction, decoding, or client rendering. Client JSON
serialization charges the retained input and output together, and writes capture
runs directly without constructing a second JSON object graph. Plain text uses
one grant for the entire value too: nested lists stream their captures and other
answers into one admitted output, including formatter workspace and text imports.
Shared references are conservatively charged at each occurrence.
Builders charge capacity before reserving, including the old and new
blocks during growth. Captures admit selected cell work before traversal,
then charge text, runs and wire bytes as they build. Ordinary trimmed
captures skip synthesized trailing blanks; full captures keep them.
The cell allowance is twice `history-bytes`, the live screen, and twice
the wire allowance for synthesized output cells. A joined range's prefix
costs row metadata, and `last-output` charges only its selected rows.
These caps admit the measured 21.9 MB default-history styled capture.
A larger observation fails explicitly; it is never clipped. Request fewer
history rows or a narrower row range to reduce a capture. A malformed or
oversized reply ends that request's connection.

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
{"pane":"%4","version":"%4:0.12.0.0.1:RUN","changed":["screen"],"exited":5,"signal":null,"status":0}
{"pane":"%5","version":"%5:0.9.0.0.1:RUN","changed":[],"exited":null,"signal":15,"status":0}
{"pane":"%2","version":"%2:1.3.0.0.0:RUN","changed":["respawned"],"status":0}
```

`changed` lists `screen`, `bell` and `done` (a count of the version that
grew), or `respawned` (the program started again: its counts but
`RESPAWNS` start afresh). When the program ended, both `exited` and `signal`
are present, with inapplicable values `null`. `exited` is the program's status
if it ended since (`null` if it was signalled or its status was lost),
`signal` the signal that killed it; a wait from a version that already holds its end waits for
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

## `all events`, `X events`

One object a line: `{"event":NAME,"pane":"%3","tab":"@2","workspace":"+2",…}`,
`pane`, `tab` and `workspace` each only where it applies: none of them
on `ready`, `missed`, `gone` or `stopped`. Events come in the
order they happened in the whole session, one line each: each is written
to the session's journal where it happens, and a stream reads the journal from
its own place. A reader that falls behind is told the rest as it reads.
A stream starts from now, or with `from` from a cursor, `SEQ:RUN`, as
`ls` and `ready` give it (`all str CURSOR from events` for everything,
`X str CURSOR from events` for X, which must still be there). A cursor
of another server run, or past the journal's end, is refused
(`bad_value`).
The journal keeps the session's last 16,384 entries, for every stream
alike: a pane that makes many in one round (a flood of bells) can make
even a stream that reads at once, of another pane, miss some, which
`missed` says.

| `event` | Also |
| --- | --- |
| `ready` | first: `from`, the cursor it reads from; from there on nothing is missed unsaid |
| `gone` | last: everything `events` named has closed (status 4) |
| `created` | `name`, for a tab or workspace, `null` when unnamed |
| `renamed` | `name`: a pane's, tab's or workspace's new name |
| `closed`, `bell` | |
| `arranged` | one complete tab layout replacement; `tab`, `workspace`; pane resizes follow |
| `respawned` | at every respawn. Its fields, only when the program it ran over had not ended (a `kill-first` respawn), as one that ended said them with its `exited`: `truncated` (`true`) if it left more than 1 MiB on its terminal: the rest was not read; `input_unread`, if any: bytes of input fux queued for it and had not yet written to its terminal, let go |
| `moved` | `from_tab`, `from_workspace`: where the pane was; `events` either place tells it |
| `exited` | `exited`, its status (`null` if it was lost), or `signal`: its program's end, as a pane's JSON and a wait say it; `truncated` (`true`) if it left more than 1 MiB on its terminal as it ended: the rest was not read; `input_unread`, if any: bytes of input fux queued for it and had not yet written to its terminal, let go (as a pane's `input_unread`); a shell's pane then closes (`closed`) |
| `done` | `status`: a shell command's, `null` if the shell gave none |
| `notification` | `text`, `truncated` (`true`) if cut as a pane's `notification` is |
| `title` | `title`, `truncated` (`true`) if cut as a pane's `title` is |
| `resized` | `cols`, `rows` |
| `history_budget` | `history_bytes`: the pane's new budget, even if no rows were evicted |
| `missed` | `count`: entries the journal let go (past what it keeps) before this stream was told them, or, from a cursor, entries after it let go before the stream began; with `events`, some may be of other things |
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
  `input_full`, `stale`, `no_marks`, `start`, `size`, `memory`, `mouse`, `signal`,
  `not_headless`, `own_server`, `people_attached`;
- how a wait ends: `timeout` (124), `closed` (4), `gone` (4);
- a program of words: `steps`, `cancelled`, `not_here`, `not_config`,
  `config`, `bad_binding`, `check`, `read`, `unset_var`, `output`, `nested`,
  `not_a_key`;
- the language's own: `require`, `open_quote`, `unknown_word`, `missing_token`,
  `bad_name`, `not_top_level`, `unmatched`, `unclosed`, `stack_underflow`,
  `stack_overflow`, `call_depth`, `type`, `div_by_zero`, `overflow`,
  `unset`, `unused`, `range`, `fail`, `too_long`, `bad_codes`, `contract`.

A program catches the failures its words give, by code, with `try` (`fux
help try`); a code no running word gives is refused before it runs. Of the
language's own, fux's words give `unused`, `unknown_word` and `bad_name` as
they run, so a `try` may name them too.

## `status json`

```json
{"socket":"/run/user/1000/fux/server.sock","release":"0.36.0","protocol":46,
 "server_release":"0.36.0","server_protocol":46,"compatible":true}
```

With no server, `status` fails (`no_server`), saying the socket and whether
a server that stopped left it there. Every server in fux's directory:
`status json` on each socket there (`fux help servers`).

`buffers` returns an array of metadata objects, newest first, for example
`[{"id":"b7:48211-1867a3f9c2b8e7d6","bytes":7,"preview":"hello  "}]`.
`bytes` is the exact UTF-8 byte length; `preview` contains at most 50 characters,
with control characters replaced by spaces. The full text is returned only by
`str ID buffer`. IDs include the server run and are never reused in that run;
an evicted ID returns `no_buffer`; an ID from another run is refused with `bad_value`.
