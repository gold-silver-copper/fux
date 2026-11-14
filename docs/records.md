# Every record fux gives

fux's facts are grift data. This page says what each record and each field means;
[`records.txt`](records.txt) lists the fields of every record in order, generated from the code,
and [`codes.txt`](codes.txt) every failure kind with its exit status.

## How a value prints

A value left by the last form of a program prints on stdout. Inside a program, `get` reads one
field: `(get (pane "sh1") "program")`.

| Value | Prints as |
| --- | --- |
| a record | `((field value) ...)`, one line, fields in the order [`records.txt`](records.txt) gives; grift's `read-form` reads it back and `get` reads a field of what it read |
| a string | its text, alone at the end of a program; `"quoted"` inside a record |
| an integer, a boolean | itself: `42`, `#true`, `#false` |
| nothing there | the symbol `none`, distinct from the empty list `()`, from `#false` and from `""` |
| a list | one item a line when it is the program's final value; `(a b c)` inside a record, or under `data-text` or `display` |
| a handle | alone, its bare id: `%3`, `@2`, `+1`, `c1`. Inside a record, or under `data-text`, a symbol that holds its server run: `%3:48211-18e5...`, `@2:RUN`, `w+1:RUN` for a workspace, `c1:RUN` |
| `(all)` | `%all`, where a `scope` field holds it |
| a capture | its text. `(data-text capture)` or `(datum capture)` is the record |
| an answer | its text, a version and what changed. `(data-text answer)` or `(datum answer)` is the record |
| a request | under `data-text`, `(request! SUBJECT (OPTION ARG...)...)`: what was asked for, so far not done, as `(axis (pane 3) "horizontal")` is before `split` takes it. One left at a program's end, its action never run, fails `unused` |

`(string value)` is the text a value prints as on its own: a handle's is its bare id. Rows and
columns count from 0; times are milliseconds. A record's field and an error's kind use hyphens:
`layout-revision`, `process-id`, `no-pane`. A string that looks like `%3` stays a string; `(pane
"%3")` makes it a handle.

A server's answer or a capture is a record with a `kind` field: `capture`, `client-capture`,
`answer` or `closed-answer`. It also has `run`, the server run that answered, and an answer has
`text`, what it prints as. `(kind value)` says what any value is: `pane`, `tab`, `workspace`,
`client`, `all`, `record`, `capture`, `client-capture`, `answer`, `closed-answer`, `pane-request` and
the like for a request, `list`, `string`, `integer`, `boolean`, `symbol`, `none`, `code`,
`applicative` and the other kinds of grift's own.

fux sets no limit of its own on how large a value is: the user and the OS do (`ulimit`, cgroups).

## A pane

A pane's facts are one observation, read in one server turn: its identity and place, its program,
what it was started as and its terminal's state, in one flat record, the same wherever it is read.
Each pane runs in a process of its own, which holds its terminal and reports to the server in
order: what `get` and a snapshot read is the server's copy, as the pane's process last reported it;
a capture asks the pane's process, and reads it as it is.

`(information (all))` and `(information X)` give each pane's under `panes`; `(capture PANE)` gives
it with what it read of the screen and the journal's `cursor` (below); `(get PANE "FIELD")` reads
one field of it now. In order:

| Field | Value |
| --- | --- |
| `id` | the pane, a handle |
| `tab` | the tab it is in, a handle |
| `version` | its version, the string `%3:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN`: its pane, its counts and the server's run (`(explain "agents")` says what each counts) |
| `respawns`, `screen`, `bells`, `done`, `ended` | its version's counts, by name: its program's respawns; its screen's revision; bells and notifications rung, shell commands ended (OSC 133 `D`) and whether it ended (0 or 1), since it started |
| `name` | its given name (`rename`), else `none`; no other workspace, tab or pane has it |
| `label` | display text: its given name, else title, launch program basename, or id; never a lookup name |
| `title` | what the program set (OSC 0 or 2), else `""`, without control characters |
| `rows`, `columns` | its size |
| `row`, `column` | where it is in its tab, laid out on the tab's effective size; `none` where it has no place |
| `program` | the program it holds, one record (below): the one it runs, or the one that ended, till a respawn's new program takes its place; `none` while a respawn lets go of a program that still ran, as the pane then holds none |
| `launch` | what it was started as: `choice`, `"shell"` or `"program"`, and `directory`, the directory it started in (`((encoding "utf8") (text "/src"))`; `"hex"` and its bytes for a directory that is not UTF-8); its program's `arguments` say with what |
| `alternate-screen` | its program holds the full screen |
| `mouse-mode` | the mouse reports its program asked for: `"press"`, `"press_release"`, `"button-motion"`, `"any-motion"`, `"highlight"`, or `none` |
| `mouse-encoding` | how it asked for mouse reports to be spelled: `"default"` (coordinates as single bytes), `"utf8"` (1005), `"sgr"` (1006) or `"urxvt"` (1015) |
| `bracketed-paste` | its program asked for bracketed paste |
| `focus-events` | its program asked to be told when focus comes and goes (1004) |
| `application-cursor` | cursor keys send their application sequences (DECCKM, `CSI ? 1 h`) |
| `application-keypad` | the keypad sends its application sequences (DECKPAM, `ESC =`) |
| `keyboard-flags` | the kitty keyboard protocol flags in force (`CSI > FLAGS u`), 0 for legacy keys |
| `pictures` | the pictures it shows (sixel), oldest first, a newer one covering the pixels of an older one that its own lie on, each at cell `row`, `column`, over `rows` by `columns` cells, the first `cropped` rows gone (scrolled off the top of a screen or region that keeps no history), `width` by `height` pixels |
| `lost` | what fux let go of its program's since it started, each kind counted (`((copies-cut 0) (copies-not-text 0) (pictures 0) (memory 0))`): `copies-cut` and `copies-not-text`, its copies (OSC 52) that copied nothing, longer than the 64 KiB a string may be, or not base64 (RFC 4648, padded or not) of UTF-8 text (none is counted for a pane created with `program-clipboard` false, which copies nothing); `pictures`, sixel pictures it drew that were not kept whole, each counted as it ends: begun while its host drew no sixel (its tab not shown, or its host draws no sixel or said no cell size), ended once it did not, lifted past a scroll region's top as it was placed, or cut: what it drew past what a picture may keep (4,096 pixels a side, 8,847,360 pixels in all, and the pictures' memory budget) was not kept, placed or not. One that drew nothing, or that CAN ended, is none; one placed whole and let go later, the oldest first as newer ones need the room, is not counted, as history let go is not; `memory`, times reading its output, or a resize, was refused memory (fux-vt's limits, `fux-vt/THREATS.md` 1.3), each losing only what it was for: history a resize let go counts too, and a size refused counts once until another size is asked or the pane keeps its own. A client showing it is told each kind, with how many, once a run of losses of that kind, which a success of the kind ends: a run of copies ends at a copy made; of pictures, at one placed whole; of memory, at a read of its output or a resize that refused none (a read: a frame and what follows it, or output once all of it is read); a program's end or a respawn starts every run anew |
| `cell` | the size of a cell in pixels its program was last told, `((width 9) (height 18))`: its tab's host's (`CSI 16 t`, asked again each time that terminal is resized, as a font zoom resizes it); `none` while it is told none (no terminal shows its tab, or its host said none): then it places no pictures |
| `cursor-visible` | the cursor shows |
| `history-bytes` | the pane's current history budget, retained across respawn |
| `program-clipboard` | whether this pane accepts program OSC 52 copies; fixed at creation |
| `reflow` | whether primary-screen lines rewrap on resize; retained across reset and respawn |
| `alternate-scroll-lines` | the ordinary wheel step when the program enables alternate scroll; 0–32767 arrows |
| `picture-bytes` | the creation picture budget, 0–96 MiB; zero disables picture support |
| `history-lines` | lines of history the screen shown holds: 0 on the alternate screen, though the main screen's is kept (`(capture (all-history (main-screen PANE)))` reads it) |
| `command-running` | a shell command runs (OSC 133 marks) |
| `last-status` | the last shell command's status, else `none` |
| `last-output` | where `last-output` reads, `((start (ROW COLUMN)) (end (ROW COLUMN)))`: from the last command's output start mark (OSC 133 C) up to its end mark (D), a running one's through its cursor's row, each end within its row's width (a resize may leave a mark past it: then the range is empty there, `start` and `end` alike); rows count from the screen's top, those above it (in history) negative. `none` where no command was marked, or its rows are gone (let go from history, or under the alternate screen) |
| `notification` | the last notification's text, else `none`, without control characters |
| `truncated` | which of `"title"` and `"notification"`, in that order, were cut, else `()`: the fields above that the program sent more of than fux-vt keeps of a string (64 KiB), the rest dropped (`("notification")`, `("title" "notification")`) |
| `input-queued` | bytes of input fux accepted for its program and has not yet written to its terminal |

A pane's `program` is one run of the process fux started in it, as its terminal's session leader:
the pane and its `generation` name it, as `FUX_PANE` (`%3:GENERATION`) does. A respawn starts the
next generation; until its program takes the pane's place, an ended program's record stays, its
status with it:

| Field | Value |
| --- | --- |
| `pane` | its pane, a handle |
| `generation` | how many times the pane was respawned before it started, its version's RESPAWNS |
| `arguments` | the checked program and arguments it was started with, which `respawn` and `dump` start again |
| `process-id` | its process, `none` once it ended |
| `state` | `"running"`; `"hung_up"`, it closed its terminal and runs on: keys are refused (Linux says so; macOS, where fux holds the terminal open, does not); `"read-failed"`, reading its terminal failed, or its pane's own process ended (`read-error`): it is no longer read and refuses keys; `"ended"`, its version's ENDED is 1 |
| `exited` | the status it exited with (a pane started with a program, not a shell), else `none` |
| `signal` | the signal that killed it (its number), else `none`; both `none` for one that ended whose status was lost |
| `read-error` | an unexpected PTY read failure as errno text, or how its pane's own process ended (`its pane's own process was killed by signal 9`), or `none`; it stays after its exit |
| `input-unread` | bytes of input fux queued for it and had not yet written to its terminal as it ended, let go, else 0. Input written to its terminal that it never read is not counted: the system does not say |
| `foreground-group` | the terminal's observed foreground process group id `G`, or `none` if unknown; its leader may have exited while other members still run. The OS says what it runs: `ps -o comm=,args= -p G`, `/proc/G/cmdline`, `readlink /proc/G/cwd` and `pgrep -a -g G` (every member) on Linux; `ps -o`, `lsof -a -p G -d cwd -Fn` and `pgrep -lf -g G` on macOS. Each is a separate read that may race with the group changing |

## Snapshots and held records

`(snapshot SCOPE PROJECTION)` returns one immutable record. `SCOPE` is `(all)`, a pane, tab,
workspace or client. `PROJECTION` is `"facts"`, `"topology"` or `"recipe"`. `(information X)` is a
facts snapshot of X, and `(list-session)` of everything. `(listing SNAPSHOT)` renders the hierarchy
from the held record as text, a pane's exit as `exited N`, a signal's as `signal NUMBER`, and an
unknown outcome as `ended, its status lost`.

Every scope has the same envelope: `run`, `cursor`, `scope`, `projection`, `workspaces`, `tabs`,
`panes` and `clients`. The four collections are flat, in hierarchy order: each workspace before the
workspaces in it, and the tabs in the order they are shown. A workspace's `children` is an ordered
list of its tab and workspace handles; a workspace's `parent`, a tab's `workspace` and a pane's
`tab` identify their parents. Ids in a record are actual handles carrying this server run.

```grift
(define! held (snapshot (all) "topology"))
(for-each (lambda! (p) (print (string (get p "id")))) (get held "panes"))
(for-each (lambda! (c) (print (string (get c "pane")))) (get held "clients"))
```

`get` on a record, a held answer or a capture is local, even if the source pane has changed,
respawned or closed. A held answer's `pane`, `version`, `status` and applicable change and exit
fields come from that answer. A capture exposes its pane's saved observation (`id`, `version`,
`last-status` and the rest) or its `client` and screen size, its journal cursor, lines and optional
styles. An absent field fails `no-field`; it never falls back to a live read. Use `(get (pane
ANSWER) "name")` to ask for the live pane's name. `(get X "FIELD")` on a live handle and
`panes`/`tabs` remain native selective reads. `id` and container membership are typed handles.

Lists stay lists, a record reads back with the same fields, booleans and integers keep their types, and strings keep
their exact text. `none` is neither false nor an empty string or list. Unsigned numbers above i64
fail `range-error`.

Each workspace has `id`, `parent` (the workspace it is in, `none` at the top), `name`, `label` and
`children`, its tabs and workspaces. Its `children` is always the full ordered membership, including
in a tab- or pane-scoped snapshot, whose `tabs` collection contains only the selected tab. Each tab
has `id`, `workspace`, `name`, `label`, `fit` (`"fixed"` or `"clients"`), `columns`, `rows`, `host`,
`nodes`, `layout-revision` and `zoomed`: the pane that fills the tab on every screen showing it,
each client focusing it there, or `none`. Client fit reports its retained size.

`layout-revision` is `TAB:REVISION:RUN`, the native layout revision, or `none` if its counter is
exhausted. Screen output does not change it. `nodes` uses the shared flat layout schema: the root
is index zero, a leaf is a pane handle, a split is `(AXIS FIRST-SHARE SECOND-SHARE FIRST SECOND)`,
and a stack is `("stack" (CHILD ...))`, two or more children each laid out on its whole rectangle,
of which each client is shown one. The axis is `"horizontal"` or `"vertical"`; shares are positive
integers and children are indices in that same list. These values feed `arrange` directly. Every
node is reachable once, with one leaf per pane. Layout depth does not consume record nesting. Each
change of a tab's layout (a split, a close, a move or swap, `arrange`, a reshape, its fit or size,
the clients' screens giving it another) emits one `laid-out` event with its new `layout-revision`;
one with another host, its layout as it was, emits one at the same revision.

Each pane is its observation (above): `id`, `tab`, `version`, its counts, `name`, `label`, `title`,
`rows`, `columns`, `row` and `column`, then, in facts and recipe, `program`, `launch` and its
terminal's fields. Topology stops before `program`: it uses only retained state and performs no
process metadata queries. Recipe adds `directory` last, where the pane's program is now, which
`dump` starts a shell in: `none` when unknown, `((status "known") (path ...))`, or `((status
"failed") (operation "cwd") (errno N))`. OS text is `((encoding "utf8") (text ...))`, or `((encoding
"hex") (text ...))` for exact non-UTF-8 bytes: a Unix path's bytes, and on Windows a path's WTF-8
bytes (UTF-8, with a lone surrogate as its three bytes). Replay launches new processes; these are
launch inputs, not saved process memory. The snapshot's `cursor` is its panes' journal cursor.

Each client has `id`, `kind` (`"terminal"` or `"headless"`), `terminal`, `output`, `read-only`,
physical `rows`/`columns`, `pane-rows`/`pane-columns`, optional `workspace`/`tab`/`pane`, `seats`,
`choices` and `opening`: the screen it shows over its panes, `"column"` (the prefix's command
column), `"repeat"` (a repeat mode's bar), `"list"` (a chooser) or `"prompt"`, `none` when none is
open. Each of `seats` is what it is shown `in` a workspace (the server's, with `in` `none`) or
focuses in a tab: `current`, and `last`, the one before. Each of `choices` is its child at a stack
of a tab in the snapshot: `stack`, `current` and `last` index that tab's `nodes` at its
`layout-revision`.

`(all)` includes every root and view. A workspace includes its descendants, the workspaces in it at
any depth with theirs, and views showing any of them; a tab includes its whole tab and parent
workspace reference. A pane includes that pane alone, in every projection: its tab as far as the
pane is in it (the tab's `nodes`, the pane alone), and relevant views. A client includes that client and its current
workspace closure. Missing explicit scopes fail.

`cursor` is the journal cursor `SEQUENCE:RUN`, gathered with the hierarchy in one server turn. A
batch from it (`(next-events (all) CURSOR 0)`) starts after that observation; a missed journal prefix
requires another snapshot. OS observations happen during the turn but are not simultaneous OS
snapshots. Each pane reports from its own process, so a snapshot holds each pane as the server last
heard from it: it claims no moment the panes shared, and one pane's facts may be newer than
another's. So a pane's `rows` and `columns` may still be its size before the last layout change, the
tab's already the new one, until the pane reports: read a pane's size from its capture, which waits
for that report. A pane version does not certify process metadata or client focus.

A snapshot is one observation, never truncated or combined from several turns; narrower scopes are
separate observations. Records nest at most 32 deep.

A tab's `host` is the terminal client supplying its panes' capability answers, or `none`. Of
terminals showing it, the last typist wins, else the first attached. Headless clients never host.
No capabilities are combined across clients; an unanswered query stays unknown even if a different
client answered.

Each client's `terminal` preserves evidence; `output` describes what fux will attempt on that
client, not what the terminal acknowledged or displayed. Headless clients have no terminal reports.

| Field | Value |
| --- | --- |
| `terminal`'s `sixel-reported` | `#true` or `#false` from DA1; `none` before any reply |
| `terminal`'s `cell-reported` | last reported `((width 10) (height 20))` in pixels, even zero or a size refused for graphics; `none` before any reply |
| `terminal`'s `underline-styles-reported` | `#true` after positive evidence; otherwise `none`, since the decoder supplies no negative report |
| `terminal`'s `kitty-flags-reported` | last decoded keyboard flags, including 0; `none` before any reply |
| `output`'s `graphics` | `"text"` without an accepted cell size; `"cell"` with a cell but no positive sixel report; `"sixel"` with both. A picture still requires the pane host's support and the same cell size on the displaying client |
| `output`'s `underline-styles` | whether styled underlines are emitted; `#false` emits plain underlines |
| `output`'s `clipboard` | `"osc52"` if this client's `terminal-clipboard` permits output, else `none`; not a delivery acknowledgement |
| `output`'s `kitty-flags-requested` | 5 once fux has requested disambiguate and alternate keys, else `none`; distinct from the last flags reported |

Cell reports are asked again on resize; the previous report stays until another arrives. These
fields and `host` are in the same bounded listing, not a separate capability query. They can change
without a pane version changing; the journal does not promise a complete stream of terminal replies.

## A capture

```grift
(data-text (capture (pane 1)))
```

```text
((kind "capture") (id %1:RUN) (tab @1:RUN) (version "...") PANE FIELDS... (cursor "SEQUENCE:RUN")
 (above 0) (wrapped-rows (#false ...)) (links (((row 0) (column 2) (length 4) (uri "https://...")))
 (screen-cursor (ROW COLUMN)) (lines ("...")) (run "RUN"))
```

A pane's capture is its observation (the pane's fields above, read in the same turn as its screen),
with what it read of the screen, and `wrap-pending`. Its `rows` and `columns` are its screen's
size. It prints as its text; `(get CAPTURE "lines")` is the list of lines, `(string CAPTURE)` the
text as one string.

`(since PANE VERSION)` reads only the lines with a row changed after that version of the pane, a
blank one too, and adds `changed-rows`, `(3 7)`, after `lines`: the row each line begins on,
numbered as `rows` counts rows (as text, each line is written after it, `3:text`, as `grep -n`
writes lines). `wrapped-rows` and `links` are of the rows of those lines alone. `version` is the
version to read from next. A version of another server run, or another pane's, fails `bad-value`.
It fails `stale`, and you capture the pane whole, where fux can't say which rows changed: the pane's
program respawned since, on a new screen; its rows moved since, where no record says (a scroll, of
the screen or of a region of it, a resize, a reset, a switch of screens or a change to history); or
it was read while a repeat (REP) was still being drawn, so it names screens whose rows differ.

`cursor` is the journal cursor as the server took the pane's answer, in the order the pane reported,
before anything it reported after the capture: `(next-events PANE CURSOR SECONDS)` or `(events PANE
CURSOR)` from it starts reading the pane's journal entries after the capture, none from before it
(as from any cursor, entries evicted first are `missed`). A change only to the screen is no entry:
it shows as a new `version` and in captures. The capture goes from the pane's own process to the
asking connection's, never through the server.

`above` is how many history rows are kept above the first row read: history not asked for (`none`
for `last-output`; rows a `rows` range leaves out are not counted).

`wrapped-rows` and `links` are of the rows the capture read, and only those (`since`: of the lines
it reads). `wrapped-rows` holds each row's `#true` where its line goes on in the next row (the rows
a joined capture joins), in order: with `wrapped`, a row each of `lines`; joined, a `rows` range
reads each line whole, so it may begin above the range. `links` holds a run of cells side by side in
one hyperlink (OSC 8) each, of the columns read: on row `row`, numbered as `rows` counts rows (the
screen's rows, without `history` or `rows`), from column `column` (the row's own, counted from 0),
`length` cells long, its target `uri`; a link that wraps has a run on each row. A link's text is what
the screen shows; its `uri` is not. A URI with a byte outside printable ASCII (a raw `café`), over
2083 bytes, or with an id over 250, links nothing: its text shows unlinked. A program percent-encodes
such bytes, each UTF-8 byte as `%XX` (`caf%C3%A9`), and the text may stay Unicode.

`lines` join a line wrapped at the pane's width whole, unless `wrapped` (each screen row a line).
Blanks at a line's end are trimmed, but with `styles` those painted (a background, reverse video,
underline or strikeout) stay, in `lines` and `styles` alike: a painted surface shows whole.
`screen-cursor` is `(ROW COLUMN)` on the screen, from 0, as a mouse event and `wrapped` count them,
whatever `lines` join or `rows` and `columns` take: `none` while hidden or on the screen not captured
(`main-screen` under a full-screen program). `wrap-pending`: a glyph went into the last column,
autowrap on, and the next goes on the next line, though the cursor shows on that last cell. With
`marks`, each line holds its marks as text does: `⟦this⟧` highlighted (reverse video, or a background
colour, as a program draws a selection), `⟨this⟩` rendered faint (SGR 2), inside any highlighting.
With the `styles` option, `styles` holds a list per line of its runs of cells that are not plain:
`((column 0) (length 3) (fg 1) (bg "#1e1e20") (bold #true) (inverse #true) (hidden #true) (dim #true)
(italic #true) (strikeout #true) (underline #true) (blink #true))`, each attribute only when set;
colours are palette numbers or `"#rrggbb"`.

`rows` and `columns` select half-open ranges, `start..end`. Equal endpoints select nothing; ranges
past available rows or columns are clipped to the available data. Negative, reversed, or endpoints
past 65535 fail with `range-error`. Rows count from the top of the requested screen and history;
joined captures expand nonempty row ranges to whole logical lines. `wrapped` keeps only the physical
rows selected. Cutting a wide glyph at either column boundary leaves a blank in the selected half.

`(capture CLIENT)` has kind `client-capture`, with `client`, `screen-rows`, `screen-columns`,
`screen-cursor`, `lines` and `run`, and no version: `screen-rows` and `screen-columns` are its
terminal's size, the client's screen composed as its own connection composes it, from what the
server says it is made of and its panes' screens as they are now, by the asking connection, which
gets them from the panes.

`data-text` writes a capture one row at a time, without a second copy of the whole record, and plain
text streams a final list's captures and other answers into one output. A capture reads each line
once into reused room and keeps an exact copy of it, its runs and the styles they name. Ordinary
trimmed captures skip synthesized trailing blanks; full captures keep them. A capture is never
clipped. A malformed reply ends that request's connection.

## What an action made

`split`, `stack`, `new-tab` and `respawn` leave an answer (below), which is also the new pane: a word
that wants a pane takes it as one, and `wait` waits from its version. `(panes X)` and `(tabs X)` are
lists of handles in layout order, and `(information X)` is all of it.

## Answers and waits

An action's answer (`send-keys`, `send-mouse`, `signal`, `split`, `stack`, `new-tab`, `respawn`, and
`line`, `type` and `press` over `send-keys`) holds:

```text
((kind "answer") (pane %3:RUN) (version "%3:0.57.0.3.0:RUN") (respawns 0) (screen 57) (bells 0)
 (done 3) (ended 0) (status 0) (text "%3:0.57.0.3.0:RUN") (run "RUN"))
```

(the version's counts by name, as a pane's): its pane's version from just before it acted, or as a
new pane starts: one answer for an action or wait on a pane, a list of them for one on a tab or
workspace, however many panes it has or answered. A word that wants a pane takes one answer, never
such a list: name the tab again, `(wait (tab 1))`.

A `wait` answers each pane whose version differs from the one it waited from (its program's end too):

```text
((kind "answer") (pane %3:RUN) (version "%3:0.61.1.4.0:RUN") ... (status 0) (changed ("screen" "bell" "done")) (text "%3:0.61.1.4.0:RUN screen bell done") ...)
((kind "answer") (pane %4:RUN) (version "%4:0.12.0.0.1:RUN") ... (changed ("screen")) (exited 5) (signal none) (text "%4:0.12.0.0.1:RUN screen exited 5") ...)
((kind "answer") (pane %5:RUN) (version "%5:0.9.0.0.1:RUN") ... (changed ()) (exited none) (signal 15) (text "%5:0.9.0.0.1:RUN killed by SIGTERM") ...)
((kind "answer") (pane %2:RUN) (version "%2:1.3.0.0.0:RUN") ... (changed ("respawned")) (text "%2:1.3.0.0.0:RUN respawned") ...)
```

`changed` lists `screen`, `bell` and `done` (a count of the version that grew), or `respawned` (the
program started again: its counts but `respawns` start afresh). When the program ended, both `exited`
and `signal` are present, with inapplicable values `none`. `exited` is the program's status if it
ended since (`none` if it was signalled or its status was lost), `signal` the signal that killed it;
a wait from a version that already holds its end waits for something else. The exit is told as the
program is reaped; what its terminal holds is read on, so `screen` and `bell` can still grow, and an
ended pane is frozen once its terminal is drained.

An answer's text is its version, then what changed, then `exited N`, `killed by SIGNAME` (or `killed
by signal N` for an unnamed signal), or `ended, its status lost` if the program ended.

A wait that fails (124 nothing changed, 4 its pane closed) answers too, with its status: an answer
with `(status 124)` and `(text "%3:0.57.0.3.0:RUN timeout")`, the version it is still at; or kind
`closed-answer`, `((kind "closed-answer") (pane %3:RUN) (status 4) (text "%3 closed") (run "RUN"))`,
its pane closed. Uncaught, it ends the program with that status, stdout getting nothing more, and says
why on stderr; nothing after it runs. Caught by `catch-kind!`, the failure's `values` hold the message
and then the answer:

```grift not run: it needs a pane 3 that does not change
(catch-kind! (wait (timeout (changed (capture (pane 3))) 5)) timeout failure
  (second (get failure "values")))
```

and `wait` on that answer waits again from the version it is still at. A wait on a tab's or a
workspace's panes answers each that changed, the first failing status naming the whole.

## `next-events` and `events`

`(next-events SCOPE CURSOR SECONDS)` returns one immutable record. Left at a program's end it prints its events alone, a line each, and a batch that missed entries is refused; read in a program it is the whole record:

```text
((events (((event "bell") (pane %3:RUN) (tab @2:RUN) (workspace w+2:RUN)
  (origin ((kind "program") (pane %3:RUN) (generation 0))) (sequence 41) (next "42:RUN"))))
 (first "0:RUN") (next "42:RUN") (missed 0) (status "continuing") (reason none))
```

The scope is a pane, tab, workspace, client or `(all)`. An empty cursor `""` starts at the journal's
current end; otherwise use `SEQUENCE:RUN`, from a snapshot's `cursor` field or a previous batch's
`next`. Other runs, future positions, and scope ids this server never issued are refused
(`bad-value`). Closed issued scopes may be replayed from a saved cursor. A pane moving between tabs
is reported to both its old and new scopes. A client scope reports changes to that client's view and
seats.

Each batch holds every matching event from its cursor to the journal's end (or to the event that
ends its scope). `next` is exactly the consumed journal prefix, including filtered entries. It can
advance with no matching events. Each event also has its own number, `sequence`, and its continuation
`next`, one past it. `first` is the oldest entry the journal still holds (`next` when it holds none):
a cursor before it has missed entries. A count of matches is never a cursor. Subject and origin
handles in a batch hold their server run, and the cursor is run-qualified text.

`SECONDS` is an explicit timeout, 0 seconds or more. With nothing to read, the connection waits until
a matching change, loss, closure, shutdown, cancellation, or its deadline. Filtered progress stays in
the pending read and is never rescanned. Zero requests an immediate read. There is one request in
flight, no server subscription registry and no timer polling loop. Cancellation closes the pending
read. Status is:

| `status` | Meaning |
| --- | --- |
| `continuing` | progress; read again from `next`, even with no matches |
| `timeout` | no matching event before the deadline; `next` includes filtered progress; no readiness claim |
| `gone` | the scoped object closed; the final close is included when retained |
| `stopped` | the server stopped; `reason` says why, and the batch ends with its `stopped` entry |

The journal keeps the last `journal-entries` entries (16,384 unless the client that started the
server set it otherwise), its ring, evicting the oldest before appending to a full ring. Retention
promises a number of entries, not seconds; scope filtering reserves no private window. `missed`
counts exactly the entries evicted before this cursor, including nonmatches. On loss, resnapshot and
reconcile, or fail the task explicitly. A full ring is evicted before append and does not grow. A
journal is its server process's, ordered as each pane's process reported: a server that takes a run
back after a crash (`(start)`) keeps one of its own, and refuses a cursor from the one before
(`bad-value`), saying that what came between is lost: resnapshot. A change to the tree (a name, a
layout, a new pane) is told only once the run's record holds it: one whose record can't be written (a
full disk) fails `client`, saying so, and is not made.

The journal guarantees ordered invalidations, not complete state replication: a later snapshot can
already contain newer state. Foreground process groups and continuous terminal cell changes are
excluded.

Known origin is supplied context, not authenticated human or agent identity. `origin`'s `kind` is
`"client"`, an attached source client named by a validated fence, independent of any client it
addresses (naming a client in an outside request does not supply its origin); `"pane"`, a command
line's caller's pane and program `generation`; `"program"`, the terminal producer's pane and
generation; `"server"`, a server action; or `"outside"`, an outside command line. Deferred respawns
retain their requesting origin. Server-observed disconnects are server actions; an explicit detach
retains its request origin. No agent label is inferred.

Client-only keymaps, request preferences and local config evaluation are not server state. Notices,
presentation preferences and edits or selection movement within an existing screen opening are not
journaled. Client records hold `opening`, and `screen` says when it changes: a fresh opening (a
replacement included) or a close, including a chooser fux closes because what it was about is gone.
Actual pane resizes and host changes still invalidate their pane or layout after a presentation
edit. Program `notification` and `title` events remain distinct from client notices.

To observe an opening or ending, arm from the client snapshot's `cursor`, then resnapshot after
`screen` and handle `missed` explicitly. A screen event is an invalidation, not an opening-success
acknowledgment or an input lock. A failed notice-only open may require a timeout or capture.
Preferences and screen contents cannot be cached solely from journal silence. Ordered keys still use
the client's existing queue. Text and full trees are not copied into layout and view invalidations.
The finite coverage schema is:

| `event` | Also |
| --- | --- |
| `created` | `name` on a tab or workspace, `none` when unnamed; a pane's `role`, `"shell"` (its exit closes it) or `"program"`; creation invalidates membership and layout |
| `renamed` | `name` |
| `closed` | `why`: `"killed"` by a `kill` of it or of what holds it, `"exited"` for its shell's exit, or `"emptied"` as its last child left; closure invalidates membership, layout and repaired views |
| `reordered` | changed tab or workspace order |
| (any of these four, and `moved`, on a workspace) | `parent`: the workspace it is in, `none` at the top; a workspace's events reach the scopes of the workspaces holding it, and a move's those of the one it left |
| `laid-out` | tab, workspace and its `layout-revision` after the change, `none` once exhausted: a split, close, move, swap, reshaping, a guarded `arrange`, its fit or size changed (a new revision), or the size it is laid out on or its effective host, as a client shows, resizes, types, leaves or shows another tab (the same one); pane resizes can follow |
| `moved` | a pane: `from-tab`, `from-workspace`; a workspace: its new `parent`; both old and new scopes receive it |
| `attached` | `client`, its place, its `kind` (`"terminal"` or `"headless"`) and `read-only` |
| `detached` | `client`, its last place if known, and `why`: `"detached"` when asked (its terminal or `detach`), `"disconnected"` when its connection closed or failed, `"server_stopped"` before the server's `stopped`, `"failed"` when the server could not paint it |
| `shown` | `client` and its place, each time it changes at any level (workspace, tab, focused pane, or a zoom of its tab): one at the place left, one at the place reached; includes automatic repair after a close |
| `zoomed` | tab, workspace and its zoomed `pane`, `none` when it no longer has one: by `zoom`, a focus on another pane there, or the pane leaving |
| `view-resized` | `client` and its terminal's new `columns`, `rows` |
| `screen` | `client` and the `kind` of screen it now shows (`"column"`, `"repeat"`, `"list"` or `"prompt"`), `none` once none: a fresh opening or a close |
| `history-budget` | pane's new `history-bytes` |
| `history-cleared` | pane history invalidated |
| `respawned` | the new program's `generation`; optional `input-unread`, the old program's input not written to its terminal |
| `hung-up` | its program closed its terminal and runs on (`"hung_up"`) |
| `read-failed` | `read-error`, the errno text of an unexpected PTY read failure, or how its pane's own process ended; then its program's `exited`, with the status its program really ended with when it can be read, else lost, and its role's rule (a shell's pane closes) |
| `exited` | `exited` (`none` when lost) or `signal`; optional `input-unread`; a shell's pane then closes |
| `input` | a request's input queued for its program, never its text: the program's `generation`, `kind` (`"keys"` from `send-keys`, `"paste"` when a paste is among them, `"mouse"` from `send-mouse`), `bytes`, and `expected`, the version `expect` admitted it at, if it did. Typing at an attached terminal is not journaled |
| `signalled` | `signal`'s number, sent by `signal` to what runs in the foreground of the program `generation` |
| `command-started` | a shell command's output began (OSC 133 `C`) |
| `done` | shell command's `status`, `none` when unreported |
| `bell` | |
| `notification` | `text`, optional `truncated` |
| `title` | `title`, optional `truncated` |
| `resized` | pane `columns`, `rows` |
| `stopped` | `reason`; the run's last entry, which every scope reads and which ends every read |

`(events SCOPE CURSOR)` is a library program over `next-events`, as `journalctl -f --after-cursor`:
it prints the scope's events after CURSOR (`""` starts now), one record a line with handles as plain
strings (`(pane "%3")`), each with its `next`, until the scope is gone (its close the last line, when
retained) or the server stops (its `stopped` entry the last line). A timeout prints nothing; it waits
again. Its consumer bounds it (`head -n`, `timeout`, `grep -m 1`) and keeps it (`>> FILE`, `tee`), and
restarts from the last line's `next`: `missed` then counts the entries evicted since, including
nonmatches. Loss fails it (`journal-loss`, exit 1) before it prints a later event; resnapshot. Effects
are not exactly once. A hook is an ordinary client program over that stream, filtering both event
and origin, with explicit source; it never runs on the server.

## Failures

A failure's first line on stderr is its cause, `fux: KIND: VALUES (FILE:LINE)`. After it comes a line
for each step that had taken effect, `fux: already took effect: (rename ...)`, then the forms it
happened in, `fux: in FORM` and `fux: from FORM at FILE:LINE`: at most eight, each cut short, as its
values are. A client's bar says the same on one line, joined by ` · `, as far as it
fits. Its exit status is 2 for a mistake in the program itself
(`usage`, `type-error`, `arity-error`, `unbound-symbol`, `read-error`, `parse-error`), a wait's own
(above), else 1. Every native kind (`fux-wire/src/code.rs` says what each means):

- reaching a server: `no-server`, `mismatch`, `client`;
- what a command names: `no-pane`, `no-tab`, `no-workspace`, `no-client`, `no-target`, `stray-pane`,
  `no-field`, `nothing-there`, `name-taken`, `bad-value`, `usage`;
- what a command meets: `not-running`, `still-running`, `not-run`, `only-shell`, `stale`, `no-marks`,
  `start`, `size`, `mouse`, `signal`, `not-headless`, `own-server`, `people-attached`,
  `journal-loss`, and `indeterminate`, a pane's own process that ended before it said whether it did what it was asked:
  it may have;
- how a wait ends: `timeout` (124), `closed` (4);
- a program: `cancelled`, `not-here`, `not-config`, `config`, `bad-binding`, `read`, `unset-variable`,
  `output`, `nested`, `not-a-key`;
- the language: `type-error`, `range-error`, `arity-error`, `unbound-symbol`, `read-error`,
  `parse-error`, `contract-error`, `bad-name`, `too-long`, `unused`, `out-of-memory`, `ids-exhausted`,
  `fail`.

The libraries raise two kinds of their own, each exit 1: `ended-before-text` (`shows`, on a program
that ended without the text) and `respawned-during-wait` (`grows`). A
program may raise any kind: `(raise (error (quote! my-kind) 1 2))`.

fux sets no limits of its own: memory, time and steps are the user's and the OS's to bound (`ulimit`,
cgroups, `timeout`), and a program that loops is stopped as any program is. At its descriptor or
process limit a server refuses a new client with the cause
(`the server can serve no new client now: …`), and a change whose record would pass its file-size
limit is refused with nothing changed. The refusal does not come, and the client waits out its own
timeout, when the whole system has no descriptor left, or when a running server's descriptor limit is
lowered below the few it opened as it started. A connection the server ends is told why first; so is
one whose server's process ended (killed, or refused memory), by the connection's own process, with
how its run is taken back (`fux '(start)'`). A connection that closes with no reason at all lost that
process, not necessarily its server: `fux '(status)'` says whether a server runs. What nests too deep to
cross the wire (a callback's lists past 64, records past 32) fails `too-long`, whether the word or
the server refuses it. So does a request longer than one frame (1 MiB), its keys or paste apart
(a name, a directory or a program of that length): its client's own connection process refuses it,
and the server is sent none of it. A pane's program and directory together are 256 KiB at most (a
quarter of a frame, as its facts say them): `new-tab`, `split` or `respawn` with more fails `too-long`. A layout is a flat list of nodes, so it nests as deep as its panes take it,
with no limit of its own. A size no screen can have, past what a grid holds, fails `size`. When the OS refuses memory where grift asks for it, the failure is `out-of-memory`, which no
handler catches. So it is where the client joins and reads a reply (a capture too large for its memory, say):
that request fails `out-of-memory`, and the next is served. Anywhere else fux's own code asks for memory
(the client printing a reply, the server, a connection's or a pane's process), a refusal aborts that process.
[`codes.txt`](codes.txt) lists every kind with its exit status.

A program catches a failure by kind with `(catch-kind! EXPRESSION KIND NAME HANDLER...)`, or any with
`(catch! EXPRESSION NAME HANDLER...)`. The handler sees one failure, a grift context; `get` reads its
`kind` (a symbol), `values` (the list of what it carries: a message, then what it holds, such as a
failed wait's answer) and `trace` (the forms it happened in). `(actions)` is the list of the steps
that had taken effect by then, as an uncaught failure lists them on stderr.

## `status`

```text
((socket "/run/user/1000/fux/server.sock") (release "0.37.0") (protocol 78)
 (server-release "0.37.0") (server-protocol 78) (server-process-id 4242) (compatible #true))
```

`(status)` returns an ordinary record with these locally readable fields, including `(compatible
#false)` for a protocol mismatch. `server-process-id` is the server's process, as the system recorded
it for the socket; where it says none (macOS), the field is left out. A command line against a server
of another protocol runs again with that process's binary.

With no server, `status` fails (`no-server`), saying the socket and whether a server that stopped left
it there. Every server in fux's directory: `(status)` on each socket there (`(explain "servers")`).

## A client's `keys` and `preferences`

```text
((at "C-b r")
 (keymap (((kind "map") (key "C-b") (repeat #false) (keymap (
   ((kind "binding") (key "v") (code (lambda! () (split (axis (focused (me)) "horizontal")) #inert))
    (label "split side by side") (group "Panes"))
   ((kind "map") (key "r") (repeat #true) (keymap (
     ((kind "binding") (key "h") (code (lambda! () (move-border (focused (me)) "left" 1) #inert))
      (label "shrink or grow left") (group "Resize"))))))))))
 (list (((key "Enter") (action "choose") (code none) (label "select"))
        ((key "r") (action none) (code (lambda! (client target) (rename-prompt client target))) (label "rename"))))
 (line (((key "Enter") (action "submit") (code none) (label "accept")))))
```

```text
((name "reflow") (value #true) (type "#true or #false"))
((name "bar") (value ("off" ("workspace" "tabs") ("pane") #false)) (type "[top/bottom/off [left fields] [right fields] tab-numbers?]; ..."))
```

A client's keys and preferences are its own: the server keeps no copy, and relays `(keys CLIENT)` and
`(preferences CLIENT)` to the client, which answers them (a client that detaches first fails
`no-client`). `at` is the keys it has typed into the map it is in now: `""` at the root, `"C-b"` with
the column open, `"C-b r"` in the resize repeat mode. `keymap` is the root's keys in the order bound,
each a `key` that runs a `binding`, with its `code`, `label` and `group`, or holds a `map` of more
keys, which says whether it `repeat`s and has a `keymap` of its own, as deep as the keys go. `list`
and `line` are the keys of fux's own screens: each its screen's `action`, or for a list's `bind-action`
key its `code`, and its `label` or `none`. `(list-keys)` prints the same keys as config lines.

A `code` is the form the binding's `code!` holds, and reading it runs nothing. It evaluates in the
program that runs it, asking nothing of the server and reading no file: a name that program lacks
fails there, `unbound-symbol`. The command line lacks a client config's own names, which a key's or
the prompt's program has. A callback, `bind-key` or another client takes it as it came.

`(preferences CLIENT)` lists each preference's `name`, its `value` as `set` takes it, and its `type`.

## `word` and `dictionary`

```text
((name "line") (provenance "library") (effect "(target text-value)")
 (documentation "line\nParameters: (target text-value)\n\n(lambda! ...)\n")
 (definition "#[applicative #[operative (target text-value) #ignore (send-keys ...)]]") (codes none))
((name "percent") (provenance "fux") (effect "n -- size") (documentation "percent\nn percent of the tab...")
 (definition "#[applicative #[host percent]]") (codes ("type-error" "unused" ...)))
((name "double") (provenance "run") (effect none) (documentation none)
 (definition "#[applicative #[operative (x) #ignore (multiply x 2)]]") (codes none))
```

`(word "NAME")` gives the record of what NAME is bound to now in the program, read locally with no
server. `(dictionary)` gives a list of every name's record. `provenance` is `core` for grift's own
names, `fux` for fux's native words, `library` for what fux's libraries define, or `run` for the
program's own (a config's too). `effect` is a native word's arguments and result, or a library word's
parameters, and `documentation` all `(explain "NAME")` says of the word; both are `none` for a name
the program defined and for grift's own. `definition` is the value as it prints: `#[host NAME]` for a
word in Rust. `codes` is what a native word can fail with as it runs, each kind once, as `catch-kind!`
names them (`docs/codes.txt`); `none`, unknown, for anything else. A word of fux's that asks the server
lists every kind asking a request can fail with (the server's answers, and the client's own as it
asks).
