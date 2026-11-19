# fux2 as built

Two crates. `fux2` is the binary and the library: `wire` (frames and messages),
`conn` (one non-blocking connection), `client` (blocking calls for the command
line), `dir` (the session directory), `model` (a terminal, its versions and
tokens; no I/O), `pane` (the daemon), `sys` (what differs between systems).
`fux2-view` is the viewer and the only crate that links grift. Both rest on
`fux-vt` and `fuxix`; neither uses `fux-pane` or `fux-wire`.

## A pane

`fux2 start` launches this program as `__pane`, in a session of its own with a
pipe on stdin and one on stdout. It tells the daemon `go`, or `go bound` when
Windows refused to let it leave a job. The daemon claims the name, opens the
endpoint and the terminal, starts the program, answers `ok <token>` or
`err <code> <message>`, and then points its standard streams at the null device.
No answer within the bound means `indeterminate`.

One poll loop serves the child-exit signal, the terminal, the listener and at most
16 connections. Order within a turn: note an exit, read output, parse it, settle
an ended program, write input, serve each ready connection once, accept, wake
waits, expire deadlines.

- **Input** is one FIFO with separate limits: 64 KiB typed, 64 KiB of terminal
  replies. A send is admitted whole or refused whole. Windows' Ctrl-C and
  Ctrl-Break go in a small urgent buffer written ahead of the FIFO.
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
- **Connections** have a five-second stall limit, and an answer has 30 seconds in
  all. At most 12 may be parked in a wait, and a newcomer at the limit displaces
  the longest-idle connection, so a read or kill always gets a slot. A connection
  given up on has its Windows write cancelled, so no thread stays behind.

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

Each pane has a directory `p-NAME` holding its endpoint: a socket on Unix, a
marker file naming a random pipe on Windows. The claim is an OS lock held for the
daemon's life, on the directory itself on Unix and on a `claim` file on Windows. A
separate `lock` file serializes changes to the directory only. Under it, a claim
that can be taken belongs to nobody, and its leftovers are removed. On Windows the
daemon runs from a copy in `bin/`, named for its contents, so the installed
program can be replaced while panes run.

## The viewer

One thread polls: keys, each pane's watching connection, and each pane's input
connection, opened at the first key. A second thread writes to the terminal, so a
terminal that stops reading delays no key; frames merge meanwhile. A third runs
grift, starting with the default keys in `defaults.grift`. A workspace and a tab are
what the viewer filters by; panes know nothing of them. Keys bound with `bind-verb`
act on the viewer's own thread, so they work while a binding runs. Other bindings
reach panes by running the `fux2` command, which can be killed when
a binding is stopped, and they name a pane as `NAME@ID`. Screens are composed as
cells first; bytes for the terminal are made only from those, with control
characters and malformed style text dropped.
