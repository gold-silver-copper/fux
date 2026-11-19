# fux2 as built

Two crates. `fux2` is the binary and the library: `wire` (frames and messages),
`conn` (one non-blocking connection), `client` (blocking calls for the command
line), `dir` (the session directory), `layout` (the stored layout, its file and
its edits; the daemon never uses it), `model` (a terminal, its versions and
tokens; no I/O), `pane` (the daemon), `sys` (what differs between systems).
`fux2-view` is the viewer and the only crate that links grift. Both rest on
`fux-vt` and `fuxix`; neither uses `fux-pane` or `fux-wire`.

## A pane

fux2 runs on Linux and macOS. `fux2 start` launches this program as `__pane`,
in a session of its own with a pipe on stdin and one on stdout, and tells the
daemon `go`. The daemon claims the name, opens the
endpoint and the terminal, starts the program, answers `ok <token>` or
`err <code> <message>`, and then points its standard streams at the null device.
No answer within the bound means `indeterminate`.

One poll loop serves the child-exit signal, the terminal, the listener and at most
16 connections. Order within a turn: note an exit, read output, parse it, settle
an ended program, write input, serve each ready connection once, accept, wake
waits, expire deadlines.

- **Input** is one FIFO with separate limits: 64 KiB typed, 64 KiB of terminal
  replies. A send is admitted whole or refused whole.
- **Output** goes to the parser 512 bytes at a time and only while 32 KiB of reply
  room is free, which covers a reply of 64 bytes for every byte. Without room the
  pane stops reading, so a program that will not read its input is slowed, not
  buffered without limit.
- **An ending** is read with `Job::ended`, which leaves the leader unreaped, so
  `kill` can still reach its session or job. The daemon then reads the terminal
  until end of file, or until it has been quiet past the settle time, at most one
  second; the state says which. The screen is frozen until `kill`.
- **`kill`** hangs up, waits 200 ms, kills the session or job, waits up to two
  seconds for the leader and everything else in it, reaps, removes the name, then
  answers. It reports anything it could not end. On Linux the daemon is the
  subreaper of what its program leaves.
- **Connections** have a five-second stall limit while an answer is unread
  (a connection that has read its answer and asks nothing is idle, and stays), and an answer has 30 seconds in
  all. At most 12 may be parked in a wait, and a newcomer at the limit displaces
  a connection idle for over a second, so a read or kill gets a slot within 5 s,
  or within 30 s when the others are slow readers of large answers (D-18).

## Versions and reads

The version advances for each turn that parsed output, each effective resize and
each change of state. Each token handed out remembers its screen's mark and row
ids, for the last 64 tokens. `read(since)` answers `unchanged`, a patch, or the
whole screen if the token is forgotten or the size changed. A patch says how many
rows moved up and carries each row whose id at the moved place differs or which
changed since, so a scroll costs its new rows. Every legal screen fits one 8 MiB
answer (`WORST_SCREEN`, checked at compile time).

History reads go by row number. Numbers run on past history down the primary
screen, and a row keeps its number as it scrolls up, so `read --history-text` sees
each row once however the program moves meanwhile. It stops at the end the first
page reported. An answer stops adding rows at 4 MiB.

A resize prepares a copy of the terminal at the new size, asks the system, and
only then installs the copy and advances the version. A refusal changes nothing.

## The session directory

Each pane has a directory `p-NAME` holding its socket. The claim is an OS lock on
that directory, held for the daemon's life. A separate `lock` file serializes
changes to the directory only. Under it, a claim that can be taken belongs to
nobody, and its leftovers are removed.

## The layout

One source of truth for each fact: the panes say what exists (the session
directory and each daemon's hello), the file `layout` says how it is arranged,
the bindings say what keys do and what the menu shows. What a viewer draws is
derived: `Layout::shown` adds each live pane without a tile to the first tab
whose filter it passes, as a grid appended to the tab's tree; geometry is
`Tab::tiles`, the same function for the viewer's cells and for the command
line's `--area`.

An edit is `layout::edit`: ask the panes who they are (before the lock, so a
slow pane holds up no writer), take `layout.lock` (a file of its own, waited
for at most two seconds, never the session's lock, so a stalled edit never
holds up `start` or `kill`), read and check the file, compare `--if`, apply
one `Op` to the layout as stored, encode it and read the text back (nothing
is written that the reader would refuse), remove whatever stands at
`layout.new` and make it afresh (private, exclusive, never through a link),
flush it, rename it over `layout`, flush the directory. Neither name is
followed if it is a link. A killed writer leaves
at most a stale `layout.new`, which nothing reads and the next writer
truncates. An edit that changes nothing writes nothing.

`Op`s that change a tab's tree first store that tab as it is shown, routed
panes and all, so they can be named; renaming, filtering and zooming store the
tab alone. While an edit places a pane (`split`, `replace`, `move`), that pane
is not routed, so a pane just started moves no other tile. What an edit
prints as `changed` is every pane whose rectangle in `--area` differs between
the layout shown before and after it. Each part of a split but the last gives
its own last cell to the border after it, so a part added, moved or resized
shifts no other part.

## The viewer

One thread polls: keys, each pane's watching connection, and each pane's input
connection, opened at the first key. A second thread writes to the terminal, so a
terminal that stops reading delays no key; frames merge meanwhile. A third runs
grift: the command library (`library.grift`), the default keys
(`defaults.grift`), then the user's `view.grift`. The viewer rereads `layout`
each second and at once after its own edits, keeps the last good one when a
read fails, and derives what it draws from it and the live panes.

The key table (`keys.rs`) holds each binding with its caption and heading, and
each group; dispatch and the prefix menu both read it, so nothing else
defines the menu. Keys bound with `bind-verb` act on the viewer's own thread,
so they work while a binding runs. Other bindings, and the answers of lists
and prompts, run on the grift thread with a snapshot of what the viewer shows
(`Context`), one at a time, five seconds each. A list or prompt is a panel the
viewer draws over the panes (`panel.rs`); its callback is kept, and the choice
runs it as a new run, so a person's thinking time is never a binding's. Keys
typed after a key whose binding runs are held until it opens a panel or ends,
at most a second, so a panel gets the keys meant for it. Each panel's
callbacks are kept until its answer runs or it is dropped (at most eight
panels), so an answer to an earlier panel is never lost in silence. Panes
and the layout are reached by running the `fux2` command, which can be killed
when a binding is stopped, and a pane is named as `NAME@ID`.

The screen is drawn as fux draws its own: no titles and no status line, a
one-cell border between tiles (stacked tiles have a border row, so a tile is
all pane), green where it touches the focused tile; the bar over the last row
only while a menu, a repeating group, a list or a prompt is open, or in its
corner for a notice or cut and hidden panes; every panel at the bottom right
above the bar. A pane never moves or resizes because a panel or the bar opens.

A mouse drag previews on a copy of the stored layout, with the same `Op` the
command line runs, and on release calls the library's `drag-border`, which
edits with `--if` the version the drag began at. Screens are composed as
cells first; bytes for the terminal are made only from those, with control
characters and malformed style text dropped.
