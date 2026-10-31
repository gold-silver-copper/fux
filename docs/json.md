# What `json` gives

(`fux help json`.) `json` turns the value on top of the stack into an ordinary
string, including results longer than 1,000,000 characters. `len`, `slice`,
`print`, and implicit command output accept it. JSON storage is counted and
reserved from the run's 128 MiB grant before writing; retained inputs and JSON
output share that grant. Explicit `string` conversion still obeys the language
string limit. All live imported storage shares the grant: captures, JSON
output, records, strings, lists and options. Reservations follow shared
allocations; clones and projected children keep their reservation without
double charging. Dropping the last reference refunds its bytes, so polling and
discarding results can continue. Per-value limits and traversal charges still
apply; the length and expanded weight checks exempt only a top-level string
already reserved in this run. Host text arguments still take at most
1,000,000 characters, checked before copying or quoting.

The JSON represents what a word left, as below; a string as a JSON string; a number or boolean
as itself; a list as an array of these, each answer in it as `json` gives
it alone (a capture's, a record's or an action's object; one of several
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
| `lost` | what fux let go of its program's since it started, each kind counted (`{"input":0,"replies":1,"copies_cut":0,"copies_not_text":0,"pictures":0,"memory":0}`): `input`, pieces of input (keys, a paste, `send-keys`, `send-mouse`, the mouse and focus reports a client sends it) its full input queue refused, which takes nothing then until its program reads; `replies`, replies to its queries (a cursor report, a colour, its size) and in-band resize reports (mode 2048) dropped: those past 4 KiB of replies to one read of its output, all of one read's while its input queue was full, or a resize report the full queue refused (a colour-scheme report it is sent unasked (mode 2031) and refused is sent again, not lost); `copies_cut` and `copies_not_text`, its copies (OSC 52) that copied nothing, longer than the 64 KiB a string may be, or not base64 (RFC 4648, padded or not) of UTF-8 text (none is counted for a pane created with `false program-clipboard`, which copies nothing; a copy made but not sent because its client was too far behind detaches that client, told why, and is not counted here); `pictures`, sixel pictures it drew that were not kept whole, each counted as it ends: begun while its host drew no sixel (its tab not shown, or its host draws no sixel or said no cell size), ended once it did not, lifted past a scroll region's top as it was placed, or cut: what it drew past what a picture may keep (4,096 pixels a side, 8,847,360 pixels in all, and the pictures' memory budget) was not kept, placed or not. One that drew nothing, or that CAN ended, is none; one placed whole and let go later, the oldest first as newer ones need the room, is not counted, as history let go is not; `memory`, times reading its output, or a resize, was refused memory (fux-vt's limits, `fux-vt/THREATS.md` 1.3), each losing only what it was for: history a resize let go counts too, and a size refused counts once until another size is asked or the pane keeps its own. A client showing it is told each kind, with how many, once a run of losses of that kind, which a success of the kind ends: a run of input ends as its program reads; of replies, as its program reads after its queue refused, or at a read that drops none while its queue takes input; of copies, at a copy made; of pictures, at one placed whole; of memory, at a read of its output or a resize that refused none (a read: a frame and what follows it, or output once all of it is read); a program's end or a respawn starts every run anew. A client typing into it is also told each key refused, as it types |
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

`cursor` is the journal cursor `SEQ:RUN`, gathered with the hierarchy in one
server turn. A batch from it (`all str CURSOR 0 next-events`) starts after
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
then read each line once into reused room and charge the exact copy kept
of it, its runs, the styles they name and the wire bytes. Ordinary trimmed
captures skip synthesized trailing blanks; full captures keep them.
The cell allowance is twice `history-bytes`, the live screen, and twice
the wire allowance for synthesized output cells. A joined range's prefix
costs row metadata, and `last-output` charges only its selected rows.
These caps admit the measured 13.0 MB default-history styled capture.
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

## `next-events` and library event streams

`SCOPE str CURSOR SECONDS next-events` returns one immutable record:

```json
{"events":[{"event":"bell","pane":"%3","tab":"@2","workspace":"+2","origin":{"kind":"program","pane":"%3","generation":0},"next":"42:7-1f"}],"next":"42:7-1f","missed":0,"status":"continuing","reason":null}
```

The scope is a pane, tab, workspace, client, or `all`. An empty cursor starts
at the journal's current end; otherwise use `SEQ:RUN`, from a snapshot's
`cursor` field or a previous batch's `next`. Other runs, future positions,
and scope IDs this server never issued are refused (`bad_value`). Closed
issued scopes may be replayed from a saved cursor. A pane moving between
tabs is reported to both its old and new scopes. A client scope reports
changes to that client's view and seats.

Each batch scans at most 256 entries, including nonmatches, returns at most
64 whole events. Bounded payloads keep a complete batch, including its
envelope, below 256 KiB of JSON; no per-event rendering is needed to admit
it. The batch also fits the ordinary finite-reply and record limits. `next`
is exactly the consumed journal prefix, including filtered entries. It can
advance with no matching events. Each event also has its own continuation
`next`. A count of matches is never a cursor. In fuxth, subject and origin
handles retain their server run; JSON renders them as short IDs for display.
The cursor and buffer ID remain run-qualified when exported as text. A run
reserves a batch's record from its allocation grant before building it:
1 KiB, plus 8 KiB per event, plus a `stopped` reason's text. Admission then
trims the reservation to the record's exact storage, so a nearly full grant
can refuse a batch whose exact record would still fit.

`SECONDS` is an explicit 0 to 86,400 second timeout. With nothing to scan,
the connection waits until a matching change, loss, closure, shutdown,
cancellation, its total scan bound, or its deadline. Filtered progress stays
in the pending read and is never rescanned. Zero requests an immediate read. There is one request in flight,
no server subscription registry and no timer polling loop. Cancellation
closes the pending read. Status is:

| `status` | Meaning |
| --- | --- |
| `continuing` | bounded progress; read again from `next`, even with no matches |
| `timeout` | no matching event before the deadline; `next` includes filtered progress; no readiness claim |
| `gone` | the scoped object closed; the final close is included when retained |
| `stopped` | the server stopped; `reason` says why; a final bounded prefix may leave unread entries |

The journal keeps the last 16,384 entries, with complete bounded payloads,
evicting the oldest before appending to a full ring. Retention promises a
number of entries, not seconds; scope filtering reserves no private window.
`missed` counts exactly the entries evicted before this cursor, including
nonmatches. On loss, resnapshot and reconcile, or fail the task explicitly.
On the measured 64-bit build, each entry occupies 80 bytes and holds at
most 2,000 bytes of compact text (a 500-character notification). Maximum
live entry storage is `16,384 × (80 + 2,000) = 34,078,720` bytes (32.5 MiB),
about 24.5 MiB above the former nominal 8 MiB entry charge. The 80-byte
slots are the deque's backing allocation, not a second set of entries.
The deque grows on demand, up to 16,384 slots (1.25 MiB); at a growth
boundary its old and replacement backing blocks can coexist, so allow
another 8,192 slots (0.625 MiB) during the final growth. The journal header
is 64 bytes, and one incoming event can hold another 2,000 bytes of text
plus its 56-byte stack value. Allocator metadata, size-class rounding and retained
allocator pages are additional. This live-entry formula is not an exact
process-heap ceiling. A full ring is evicted before append and cannot grow
for entry 16,385.

The journal guarantees ordered invalidations, not complete state replication:
a later snapshot can already contain newer state. Live cwd, foreground
process observations and continuous terminal cell changes are excluded.

Known origin is supplied context, not authenticated human or agent identity:
`client` carries an attached source client named by a validated fence, independent of
any client it addresses. Naming a client in an external CLI request does
not supply its origin. `pane` carries a CLI caller's pane and program
generation, `program` carries the terminal producer's pane and generation,
`server` denotes a server action, and an external CLI is `unknown`. Deferred
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
| `laid_out` | tab/workspace; same-tab swap, reshaping, fit or effective host changed |
| `arranged` | one guarded complete layout replacement; pane resizes can follow |
| `moved` | `from_tab`, `from_workspace`; both old and new scopes receive it |
| `attached`, `detached` | `client` and its last place if known |
| `shown`, `focused`, `zoomed` | `client` and the affected place; departures invalidate both places; includes automatic repair |
| `view_resized` | `client`; client size invalidated |
| `screen` | `client`; fresh opening, close or mode ending invalidated |
| `copied` | admitted stable `buffer` ID, no duplicate text; denied program copies emit none |
| `history_budget` | pane's new `history_bytes` |
| `history_cleared` | pane history invalidated |
| `respawned` | generation changed; `truncated` and `input_unread` describe the old program's unread output/input |
| `exited` | `exited` (null when lost) or `signal`; optional `truncated` and `input_unread`; a shell's pane then closes |
| `done` | shell command's `status`, null when unreported |
| `bell` | |
| `notification` | `text`, optional `truncated` |
| `title` | `title`, optional `truncated` |
| `resized` | pane `cols`, `rows` |
| `ready`, `missed` | library stream markers only: armed cursor, or explicit lost-entry count |

`all events` is a library program: batches until the stream ends, waiting at
most 30 seconds each, with the interpreter's step/weight limits still enforced.
`SCOPE str CURSOR SECONDS BATCHES events-since` takes a count of batches (0:
until the stream ends) and returns its final cursor. The initial immediate arming/replay read counts
toward that bound; SECONDS applies to later waits. It prints NDJSON: an armed `ready` with `from`,
then events and a checkpoint marker after every batch (its status as `event`,
`next`, `reason`). Loss prints `missed` with its count/cursor and fails before
consumer effects. Keep the last successful checkpoint to restart; effects
are not exactly once. A gone scope ends the library stream successfully; `gone` is data, not a raised error.
There is no unlimited finite-reply accumulator. Copy cursor motion is continuous
view state; opening or leaving copy mode invalidates the view.

`copy-notices` is one ordinary hook recipe over those same batches. It filters
both event and origin, notifying about admitted program copies. Notification
cannot itself generate the copied event that triggers it. See
[program-copies.fux](../examples/hooks/program-copies.fux). Hooks run as separate
client programs, with explicit source and bounded runs, never on the server.

## Errors

A failure is one line on stderr, `fux: CODE: MESSAGE`, the message naming
the file, line and word it came from. Its exit status is 1, or a
wait's own (above). CODE stays; every one fux gives
(`fux-wire/src/code.rs` says what each means):

- reaching a server: `no_server`, `mismatch`, `client`;
- what a command names: `no_pane`, `no_tab`, `no_workspace`, `no_client`,
  `no_buffer`, `no_target`, `stray_pane`, `no_field`, `nothing_there`,
  `selection`, `name_taken`, `ids_exhausted`, `bad_value`, `usage`;
- what a command meets: `not_running`, `still_running`, `not_run`, `only_shell`,
  `input_full`, `stale`, `no_marks`, `start`, `size`, `memory`, `mouse`, `signal`,
  `not_headless`, `own_server`, `people_attached`;
- how a wait ends: `timeout` (124), `closed` (4);
- a program of words: `steps`, `cancelled`, `not_here`, `not_config`,
  `config`, `bad_binding`, `check`, `read`, `unset_var`, `output`, `nested`,
  `not_a_key`;
- the language's own: `require`, `open_quote`, `unknown_word`, `missing_token`,
  `bad_name`, `not_top_level`, `unmatched`, `unclosed`, `stack_underflow`,
  `stack_overflow`, `call_depth`, `type`, `div_by_zero`, `overflow`,
  `unset`, `unused`, `range`, `fail`, `too_long`, `bad_codes`, `contract`.

A bound crossed fails `too_long` (ask for less) whether the word or the
server refuses it, a size no screen can have `size`: a layout past its
nodes or depth, callback source past its bytes, a screen past 4096 a side.
`docs/codes.txt` lists every code with its exit status.

A program catches the failures its words give, by code, with `try` (`fux
help try`); a code no running word gives is refused before it runs. Of the
language's own, fux's words give `unused`, `unknown_word` and `bad_name` as
they run, so a `try` may name them too.

## `status`

```json
{"socket":"/run/user/1000/fux/server.sock","release":"0.36.0","protocol":54,
 "server_release":"0.36.0","server_protocol":54,"server_pid":4242,"compatible":true}
```

`status` returns an ordinary record with these locally readable fields. Plain
output is JSON, including `compatible:false` for a protocol mismatch.
`server_pid` is the server's process, as the system recorded it for the
socket; where it says none (macOS), the field is left out. A command line
against a server of another protocol runs again with that process's binary.

With no server, `status` fails (`no_server`), saying the socket and whether
a server that stopped left it there. Every server in fux's directory:
`status json` on each socket there (`fux help servers`).

`buffers` returns an array of metadata objects, newest first, for example
`[{"id":"b7:48211-1867a3f9c2b8e7d6","bytes":7,"preview":"hello  "}]`.
`bytes` is the exact UTF-8 byte length; `preview` contains at most 50 characters,
with control characters replaced by spaces. The full text is returned only by
`str ID buffer`. IDs include the server run and are never reused in that run;
an evicted ID returns `no_buffer`; an ID from another run is refused with `bad_value`.
