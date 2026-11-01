# What `json` gives

(`fux help json`.) `json` turns the value on top of the stack into an ordinary
string. `len`, `slice`, `print`, and implicit command output accept it. fux
sets no limit of its own on how large it is: the user and the OS do
(`ulimit`, cgroups).

The JSON represents what a word left, as below; a string as a JSON string; a number or boolean
as itself; a list as an array of these, each answer in it as `json` gives
it alone (a capture's, a record's or an action's object; one of several
panes an array of their objects). Rows and columns count from 0; times are milliseconds; ids are strings (`"%3"`,
`"@2"`, `"+1"`, `"c1"`).

## A pane

`ls json` gives each pane its identity and size, with process fields under `state` and what it was started as under `launch` (below); `PANE capture json` gives the identity and `state` fields too, but
for `id`, `name`, `label` and `pid` (it says `pane`):

| Field | Value |
| --- | --- |
| `id` | `"%3"` |
| `name` | its given name (`rename`), else `null`; no other workspace, tab or pane has it |
| `label` | display text: its given name, else title, launch program basename, or id; never a lookup name |
| `title` | what the program set (OSC 0 or 2), else `""`, without control characters |
| `rows`, `cols` | its size |
| `pid` | its program's process, `null` once it exited, and while a respawn of it is under way |
| `version` | its version, `%3:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN`: its pane, its counts and the server's run (`fux help agents` says what each counts) |
| `foreground_group` | the terminal's observed foreground process group id `G`, or `null` if unknown; its leader may have exited while other members still run. The OS says what it runs: `ps -o comm=,args= -p G`, `/proc/G/cmdline`, `readlink /proc/G/cwd` and `pgrep -a -g G` (every member) on Linux; `ps -o`, `lsof -a -p G -d cwd -Fn` and `pgrep -lf -g G` on macOS. Each is a separate read that may race with the group changing |
| `hung_up` | its program closed its terminal and runs on: keys are refused (Linux says so; macOS, where fux holds the terminal open, does not) |
| `read_error` | an unexpected PTY read failure as errno text, or `null`: a failed reader is no longer polled and refuses keys; the cause stays after exit until respawn |
| `exited` | the status its program exited with (a pane started with a program, not a shell), else `null` |
| `signal` | the signal that killed it (its number), else `null`; both `null` for a program that ended (its version's ENDED is 1) whose status was lost |
| `alt_screen` | its program holds the full screen |
| `mouse_mode` | the mouse reports its program asked for: `"press"`, `"press_release"`, `"button_motion"`, `"any_motion"`, `"highlight"`, or `null` for none |
| `mouse_encoding` | how it asked for mouse reports to be spelled: `"default"` (coordinates as single bytes), `"utf8"` (1005), `"sgr"` (1006) or `"urxvt"` (1015) |
| `bracketed_paste` | its program asked for bracketed paste |
| `focus_events` | its program asked to be told when focus comes and goes (1004) |
| `app_cursor` | cursor keys send their application sequences (DECCKM, `CSI ? 1 h`) |
| `app_keypad` | the keypad sends its application sequences (DECKPAM, `ESC =`) |
| `keyboard_flags` | the kitty keyboard protocol flags in force (`CSI > FLAGS u`), 0 for legacy keys |
| `pictures` | the pictures it shows (sixel), oldest first, a newer one covering the pixels of an older one that its own lie on, each at cell `row`, `col`, over `rows` by `cols` cells, the first `cropped` rows gone (scrolled off the top of a screen or region that keeps no history), `width` by `height` pixels |
| `lost` | what fux let go of its program's since it started, each kind counted (`{"copies_cut":0,"copies_not_text":0,"pictures":0,"memory":0}`): `copies_cut` and `copies_not_text`, its copies (OSC 52) that copied nothing, longer than the 64 KiB a string may be, or not base64 (RFC 4648, padded or not) of UTF-8 text (none is counted for a pane created with `false program-clipboard`, which copies nothing); `pictures`, sixel pictures it drew that were not kept whole, each counted as it ends: begun while its host drew no sixel (its tab not shown, or its host draws no sixel or said no cell size), ended once it did not, lifted past a scroll region's top as it was placed, or cut: what it drew past what a picture may keep (4,096 pixels a side, 8,847,360 pixels in all, and the pictures' memory budget) was not kept, placed or not. One that drew nothing, or that CAN ended, is none; one placed whole and let go later, the oldest first as newer ones need the room, is not counted, as history let go is not; `memory`, times reading its output, or a resize, was refused memory (fux-vt's limits, `fux-vt/THREATS.md` 1.3), each losing only what it was for: history a resize let go counts too, and a size refused counts once until another size is asked or the pane keeps its own. A client showing it is told each kind, with how many, once a run of losses of that kind, which a success of the kind ends: a run of copies ends at a copy made; of pictures, at one placed whole; of memory, at a read of its output or a resize that refused none (a read: a frame and what follows it, or output once all of it is read); a program's end or a respawn starts every run anew |
| `cell` | the size of a cell in pixels its program was last told, `{"width":9,"height":18}`: its tab's host's (`CSI 16 t`, asked again each time that terminal is resized, as a font zoom resizes it); `null` while it is told none (no terminal shows its tab, or its host said none): then it places no pictures |
| `cursor_visible` | the cursor shows |
| `history_bytes` | the pane's current history budget, retained across respawn |
| `program_clipboard` | whether this pane accepts program OSC 52 copies; fixed at creation |
| `reflow` | whether primary-screen lines rewrap on resize; retained across reset and respawn |
| `alternate_scroll_lines` | the ordinary wheel step when the program enables alternate scroll; 0–32767 arrows |
| `picture_bytes` | the creation picture budget, 0–96 MiB; zero disables picture support |
| `history_lines` | lines of history the screen shown holds: 0 on the alternate screen, though the main screen's is kept (`main-screen all-history capture` reads it) |
| `command_running` | a shell command runs (OSC 133 marks) |
| `last_status` | the last shell command's status, else `null` |
| `last_output` | where `last-output` reads, `{"start":[ROW,COL],"end":[ROW,COL]}`: from the last command's output start mark (OSC 133 C) up to its end mark (D), a running one's through its cursor's row; rows count from the screen's top, those above it (in history) negative. `null` where no command was marked, or its rows are gone (let go from history, or under the alternate screen) |
| `bells` | bells and notifications rung since it started |
| `notification` | the last notification's text, else `null`, without control characters |
| `truncated` | which of `"title"` and `"notification"`, in that order, were cut, else `[]`: the fields above that the program sent more of than fux-vt keeps of a string (64 KiB), the rest dropped (`["notification"]`, `["title","notification"]`) |
| `input_queued` | bytes of input fux accepted for its program and has not yet written to its terminal |
| `input_unread` | bytes of input fux queued for its program and had not yet written to its terminal as it ended, let go (till it is respawned), else 0. Input written to its terminal that the program never read is not counted: the system does not say |

## Snapshots and local records

`SCOPE str PROJECTION snapshot` returns one immutable record. `SCOPE` is
`all`, a pane, tab, workspace or client. `PROJECTION` is `facts`, `topology`
or `recipe`. `ls` takes a facts snapshot of all scopes; `X info` takes one
of X. Plain output is JSON; `ls listing` renders the hierarchy from the held
record. Signal termination reads `signal NUMBER`; an unknown outcome is
reported as lost. `ls` still returns a record, so `ls json` works.

Every scope has the same envelope: `run`, `cursor`, `scope`, `projection`,
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
conversion. `X str FIELD get` on a live handle and `panes`/`tabs` remain native
selective reads. `get id` and container membership return typed handles.

Each workspace has `id`, `name`, `label`, and `tabs`. Its `tabs` is always the
full ordered membership, including in a tab- or pane-scoped snapshot, whose
`tabs` facts collection contains only the selected tab. Each tab fact has
`id`, `workspace`, `name`, `label`, `fit` (`fixed` or `clients`), `cols`,
`rows`, `nodes`, `layout_revision`, `host`, and `zoomed`: the pane that fills
the tab on every screen showing it, each client focusing it there, or null.
Client fit reports its retained size.

`layout_revision` is `TAB:REVISION:RUN`, the native layout revision, or null if its
counter is exhausted. Screen output does not change it. `nodes` uses the
shared flat layout schema: the root is index zero, a leaf is a typed pane
handle, and a split is `[AXIS, FIRST_SHARE, SECOND_SHARE, FIRST, SECOND]`.
The axis is `horizontal` or `vertical`; shares are positive integers and
children are indices in that same list. These values feed `arrange` directly. Every node is reachable once, with
one leaf per pane. Layout depth does not consume record nesting. A successful `arrange` emits
one `laid_out` event with the tab and workspace.

Each pane has `id`, `tab`, `name`, `label`, `title`, `pid`, `rows`, `cols`,
and `version`. A name is given or null; a label is display text. Facts adds
`state`, containing the process/terminal fields above but `title`, and
`launch`, what it was started as: `choice` (`shell` or `program`), ordered
`argv` and `cwd`, both choices' original inputs. Topology uses only
retained state and performs no process metadata queries. Recipe adds `cwd`,
where the pane's program is now, which `dump` starts a shell in: null when
unknown, `{status: known, path: ...}`, or
`{status: failed, operation: cwd, errno: N}`. OS text uses
`{encoding: utf8, text: ...}` or `{encoding: hex, text: ...}` for exact
non-UTF-8 bytes. Replay launches new processes; these are launch inputs,
not saved process memory.

Each client has `id`, `kind` (`terminal` or `headless`), `read_only`, physical
`rows`/`cols`, `pane_rows`/`pane_cols`, optional `workspace`/`tab`/`pane`,
`opening` (null without an open screen), and `mode`.

All includes every root and view. Workspace includes its descendants and
views showing it; tab includes its whole tab and parent workspace reference.
Pane facts includes only that pane's facts, with its tab topology and relevant
views; pane topology/recipe includes its whole tab. Client includes that
client and its current workspace closure. Missing explicit scopes fail.

`cursor` is the journal cursor `SEQ:RUN`, gathered with the hierarchy in one
server turn. A batch from it (`all str CURSOR 0 next-events`) starts after
that observation; a missed journal prefix requires another snapshot. OS
observations happen during the turn but are not simultaneous OS snapshots.
A pane version does not certify process metadata or client focus.

A snapshot is one observation, never truncated or combined from several
turns; narrower scopes are separate observations. Records nest at most 32
deep. Client projection keeps the native schema for plain rendering and
shares immutable records on clone.

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
{"pane":"%1","screen_rows":23,"screen_cols":80,"screen_cursor":[ROW,COL],"lines":["…"],"wrap_pending":false,PANE FIELDS…,"above":0,"wrapped_rows":[false,…],"links":[{"row":0,"col":2,"len":4,"uri":"https://…"}]}
```

`above` is how many history rows are kept above the first row read:
history not asked for (`null` for `last-output`; rows a `rows` range
leaves out are not counted).

`wrapped_rows` and `links` are of every row the capture reads from,
numbered as `rows` counts them (the screen's rows, without `history`),
whatever `rows` and `cols` take or `lines` join: so with `wrapped` alone,
`lines` holds a row each, in that order. `wrapped_rows` holds a row's
`true` where its line goes on in the next row: the rows a joined capture
joins. `links` holds a run of cells side by side in one hyperlink (OSC 8)
each, on row `row` from column `col` (a row's columns, counted from 0),
`len` cells long, its target `uri`; a link that wraps has a run on each
row. A link's text is what the screen shows; its `uri` is not.

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

Client JSON serialization writes capture runs directly without constructing
a second JSON object graph, and plain text streams nested lists' captures and
other answers into one output.
A capture reads each line once into reused room and keeps an exact copy of
it, its runs and the styles they name. Ordinary trimmed captures skip
synthesized trailing blanks; full captures keep them. A capture is never
clipped. A malformed reply ends that request's connection.

## What a word made

`split`, `new-tab`, `new-workspace` and `respawn` leave an answer
(below), which is also the new pane: a word that wants a pane takes it as
one, and `wait` waits from its version. `json` of a pane, tab or workspace is its id as a string, `"%4"`,
and `X info json` all of it. `X panes json` and `X tabs json` are arrays
of them in layout order, `["%3","%4"]`.

## Answers and waits

`json` of an action's answer (`send-keys`, `send-mouse`,
`signal`, `split`, `new-tab`, `new-workspace`, `respawn`) is
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
something else. The exit is told as the program is reaped; what its terminal
holds is read on, so `screen` and `bell` can still grow, and an ended pane is
frozen once its terminal is drained.

A wait that fails (124 nothing changed, 4 its pane closed) answers too,
with its status: `{"pane":"%3","version":"%3:0.57.0.3.0:RUN","status":124}`,
the version it is still at; `{"pane":"%3","status":4}`, its pane closed.
Uncaught, it ends the program with that status, stdout getting nothing
more, and says why on stderr, one line, `fux: CODE: MESSAGE`, then the
answers of the words before it and its own, as text; nothing after it runs. Caught by `try`,
the handler's failure carries its answer as its `value`:
`str timeout try %3 5 timeout wait catch str value get end json` prints it, and
`wait` on it waits again from the version it is still at. A wait on a
tab's or a workspace's panes answers each that changed, the first failing
status naming the whole.

## `next-events` and `events`

`SCOPE str CURSOR SECONDS next-events` returns one immutable record; its
`json`, and its fields with `get`, are all of it:

```json
{"events":[{"event":"bell","pane":"%3","tab":"@2","workspace":"+2","origin":{"kind":"program","pane":"%3","generation":0},"next":"42:7-1f"}],"next":"42:7-1f","missed":0,"status":"continuing","reason":null}
```

Left at a command line's end, it prints its events alone, a JSON object a
line (none: nothing); one with `missed` above 0 is refused there, on
stderr (`fail`, exit 1).

The scope is a pane, tab, workspace, client, or `all`. An empty cursor starts
at the journal's current end; otherwise use `SEQ:RUN`, from a snapshot's
`cursor` field or a previous batch's `next`. Other runs, future positions,
and scope IDs this server never issued are refused (`bad_value`). Closed
issued scopes may be replayed from a saved cursor. A pane moving between
tabs is reported to both its old and new scopes. A client scope reports
changes to that client's view and seats.

Each batch holds every matching event from its cursor to the journal's end
(or to the event that ends its scope). `next`
is exactly the consumed journal prefix, including filtered entries. It can
advance with no matching events. Each event also has its own continuation
`next`. A count of matches is never a cursor. In fuxth, subject and origin
handles retain their server run; JSON renders them as short IDs for display.
The cursor remains run-qualified when exported as text.

`SECONDS` is an explicit timeout, 0 seconds or more. With nothing to read,
the connection waits until a matching change, loss, closure, shutdown,
cancellation, or its deadline. Filtered progress stays
in the pending read and is never rescanned. Zero requests an immediate read. There is one request in flight,
no server subscription registry and no timer polling loop. Cancellation
closes the pending read. Status is:

| `status` | Meaning |
| --- | --- |
| `continuing` | progress; read again from `next`, even with no matches |
| `timeout` | no matching event before the deadline; `next` includes filtered progress; no readiness claim |
| `gone` | the scoped object closed; the final close is included when retained |
| `stopped` | the server stopped; `reason` says why |

The journal keeps the last `journal-entries` entries (16,384 unless the
client that started the server set it otherwise), its ring, evicting the
oldest before appending to a full ring. Retention promises a number of entries, not
seconds; scope filtering reserves no private window. `missed` counts exactly
the entries evicted before this cursor, including nonmatches. On loss,
resnapshot and reconcile, or fail the task explicitly. A full ring is
evicted before append and does not grow.

The journal guarantees ordered invalidations, not complete state replication:
a later snapshot can already contain newer state. Foreground process
groups and continuous terminal cell changes are excluded.

Known origin is supplied context, not authenticated human or agent identity:
`client` carries an attached source client named by a validated fence, independent of
any client it addresses. Naming a client in an external CLI request does
not supply its origin. `pane` carries a CLI caller's pane and program
generation, `program` carries the terminal producer's pane and generation,
`server` denotes a server action, and an external CLI is `outside`. Deferred
respawns retain their requesting origin. Server-observed disconnects are
server actions; an explicit detach retains its request origin. No agent
label is inferred.

Client-only keymaps, request preferences and local config evaluation are not
server state. Notices, presentation preferences and edits or selection
movement within an existing screen opening are not journaled. Client
records retain `mode` and `opening`; `screen` invalidates a fresh opening
(including replacement), a close or a mode ending, including repair and
history loss. Actual pane resizes and host changes still invalidate their
pane or layout after a presentation edit. Program `notification` and
`title` events remain distinct from client notices.

To observe an opening or ending, arm from the client snapshot's `cursor`,
then resnapshot after `screen` and handle `missed` explicitly. A screen
event is an invalidation, not an opening-success acknowledgment or an
input lock. A failed notice-only open may require a timeout or capture.
Preferences and screen contents cannot be cached solely from journal
silence. Ordered keys still use the client's existing queue. Text and full
trees are not copied into layout/view invalidations. The finite coverage schema is:

| `event` | Also |
| --- | --- |
| `created` | `name` on tab/workspace, null when unnamed; creation invalidates membership/layout |
| `renamed` | `name` |
| `closed` | closure invalidates membership/layout and repaired views |
| `reordered` | changed tab/workspace order |
| `laid_out` | tab/workspace; same-tab swap, reshaping, a guarded `arrange`, fit or effective host changed; pane resizes can follow |
| `moved` | `from_tab`, `from_workspace`; both old and new scopes receive it |
| `attached`, `detached` | `client` and its last place if known |
| `shown`, `focused` | `client` and the affected place; departures invalidate both places; includes automatic repair |
| `zoomed` | tab/workspace and its zoomed `pane`, null when it no longer has one: by `zoom`, a focus on another pane there, or the pane leaving |
| `view_resized` | `client`; client size invalidated |
| `screen` | `client`; fresh opening, close or mode ending invalidated |
| `history_budget` | pane's new `history_bytes` |
| `history_cleared` | pane history invalidated |
| `respawned` | generation changed; optional `input_unread`, the old program's input not written to its terminal |
| `exited` | `exited` (null when lost) or `signal`; optional `input_unread`; a shell's pane then closes |
| `done` | shell command's `status`, null when unreported |
| `bell` | |
| `notification` | `text`, optional `truncated` |
| `title` | `title`, optional `truncated` |
| `resized` | pane `cols`, `rows` |

`SCOPE str CURSOR events` is a library program over `next-events`, as
`journalctl -f --after-cursor`: it prints the scope's events after CURSOR
(an empty one starts now), one JSON object a line, each with its `next`,
until the scope is gone (its close the last line, when retained) or the
server stops. A timeout prints nothing; it waits again. Its consumer
bounds it (`head -n`, `timeout`, `jq 'first(…)'`) and keeps it (`>> FILE`,
`tee`), and restarts from the last line's `next`: `missed` then counts the
entries evicted since, including nonmatches. Loss fails it (`fail`, on
stderr) before it prints a later event; resnapshot. Effects are not
exactly once. A hook is an ordinary client program over that stream,
filtering both event and origin, with explicit source; it never runs on
the server.

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from, then a line for each step that had
taken effect before it. Its exit status is 2 for a mistake in
the program itself (`usage`, `require`, `open_quote`, `unknown_word`,
`missing_token`, `not_top_level`, `unmatched`, `unclosed`,
`stack_underflow`, `type`, `bad_codes`), a wait's own (above), else 1. CODE stays; every one fux gives
(`fux-wire/src/code.rs` says what each means):

- reaching a server: `no_server`, `mismatch`, `client`;
- what a command names: `no_pane`, `no_tab`, `no_workspace`, `no_client`,
  `no_target`, `stray_pane`, `no_field`, `nothing_there`,
  `name_taken`, `bad_value`, `usage`;
- what a command meets: `not_running`, `still_running`, `not_run`, `only_shell`,
  `stale`, `no_marks`, `start`, `size`, `mouse`, `signal`,
  `not_headless`, `own_server`, `people_attached`;
- how a wait ends: `timeout` (124), `closed` (4);
- a program of words: `cancelled`, `not_here`, `not_config`,
  `config`, `bad_binding`, `read`, `unset_var`, `output`, `nested`,
  `not_a_key`;
- the language's own: `require`, `open_quote`, `unknown_word`, `missing_token`,
  `bad_name`, `not_top_level`, `unmatched`, `unclosed`, `stack_underflow`,
  `type`, `div_by_zero`, `overflow`,
  `unset`, `unused`, `range`, `invalid_address`, `memory`, `ids_exhausted`,
  `fail`, `abort`, `quit`, `thrown`, `too_long`, `bad_codes`, `contract`;
- the language's for a host that reads input (the fuxth runner), which
  fux gives only when a program throws their numbers: `interrupt` (-28),
  `end_of_input` (-39, though -39 throws `open_quote`), `unsupported` (-21).

fux sets no limits of its own: memory, time and steps are the user's and
the OS's to bound (`ulimit`, cgroups, `timeout`), and a program that loops
is stopped as any program is. Lists nest as deep as memory lets them;
what nests too deep to cross the wire (a callback's lists past 64, records
past 32, a layout past 64) fails `too_long`, whether the word or the server
refuses it; a size no screen can have, past what a grid holds, fails
`size`. When the OS refuses
memory where fux asks for it, the failure is `memory`.
`docs/codes.txt` lists every code with its exit status, and its number in
Forth's table where it has one.

A program catches the failures its words give, by code, with `try` (`fux
help try`); a code no running word gives is refused as the `try` is read.
The handler gets one failure, which `json` prints as
`{"code":"timeout","number":null,"message":"…","value":{…},"actions":["argument 4: rename"]}`:
`number` is the code's in Forth's table of `throw` codes (`throw` takes
it), `value` what it carries, and `actions` the steps that had taken
effect, as an uncaught failure lists them on stderr after its line
(`fux: already took effect: argument 4: rename`).
Of the language's own, fux's words give `unused`, `unknown_word` and
`bad_name` as they run, so a `try` may name them too.

## `status`

```json
{"socket":"/run/user/1000/fux/server.sock","release":"0.36.0","protocol":63,
 "server_release":"0.36.0","server_protocol":63,"server_pid":4242,"compatible":true}
```

`status` returns an ordinary record with these locally readable fields. Plain
output is JSON, including `compatible:false` for a protocol mismatch.
`server_pid` is the server's process, as the system recorded it for the
socket; where it says none (macOS), the field is left out. A command line
against a server of another protocol runs again with that process's binary.

With no server, `status` fails (`no_server`), saying the socket and whether
a server that stopped left it there. Every server in fux's directory:
`status json` on each socket there (`fux help servers`).

## `word` and `dictionary`

```json
{"name":"dup","provenance":"library","effect":"a -- a a","documentation":"a copy of the top","definition":"define dup 0 pick end","codes":["stack_underflow"]}
{"name":"percent","provenance":"fux","effect":"n -- size","documentation":"n percent of the tab, for resize-pane","definition":"native","codes":["stack_underflow","type","range","bad_value"]}
{"name":"sq","provenance":"run","effect":null,"documentation":null,"definition":"define sq dup mul end","codes":["stack_underflow","type"]}
```

`str NAME word` gives the record of the word NAME names now in the
program's own dictionary, read locally with no server; a name no word has
fails `unknown_word`. `dictionary` gives a list of every word's record, the
current definition of each name once, in the order they were defined: the
core's, fux's, the libraries', then the program's own (a config's too).
`provenance` is `core`, `fux`, `library`, or `run` for the program's own.
`effect` and `documentation` are as declared, `null` for a word the program
defined. `definition` is the words that made it as they were read, from
every source that gave them (a definition begun in one line or file and
ended in the next), each quoted to be read back, then `immediate` if it is.
A `define … end` reads back as the same word. A word `create`, `variable`,
`constant`, `defer` or `let` made shows the words from its maker, the word
the reader read and ran (that word, or a defining word of the program's),
to its name; a value its maker took from the stack isn't among them, so
`constant k` alone doesn't make `k` again. `definition` is `native` for a
word in Rust, and `null` for one whose words its source couldn't keep.
`codes` is what the word can fail with as it runs, each code once, as a
`try` names them (`docs/codes.txt`). The core's words and fux's declare
theirs, each beside its effect, and a word that shapes the program (`if`,
`each`, `of`) adds what the steps it compiles fail with; a word of fux's that asks the server lists
every code asking a request can fail with (the server's answers, and the
client's own as it asks). A definition's are those of its steps and the
words it calls, but those a `try` around them catches. `codes` is `null`,
unknown, for a word that runs code it is given (`execute`, `include`,
`require`, a word `defer` made) or raises a failure it is given
(`throw`), and for a definition that calls one. A mistake found compiling
or at a program's end (`unclosed`) is no word's.
`see NAME` writes the definition, after a comment saying where it was
read, or says its source is unknown.
