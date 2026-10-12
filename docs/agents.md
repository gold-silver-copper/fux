# Driving programs with fux

(`fux help-agents`; `fux help` lists every word, `fux str WORD explain` one.)
A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed, a value a
line. The server is `FUX_SOCKET`'s; `fux start` starts one if none runs.

Every step has one of these shapes (UPPER = what you fill in):

```sh
fux opts OPTIONS run                 # start a program; prints its pane: %2
fux %2 keys INPUT [WAIT] send-keys   # type; INPUT: str T text | str Enter key | str T paste | 3 times
fux %2 opts WAIT wait                # wait; WAIT: str T wait-text | 300 wait-idle | wait-exit | … 30 timeout
fux %2 opts READ capture             # read; READ: 100 history | all-history | marks | styles | json | …
fux %2 str FIELD get                 # one value: exited (null while it runs), pid, revision, cwd
```

```sh
fux opts str app name 100 30 size str './app --fast' command str Ready wait-text run  # start; waits for Ready
fux %2 keys str j key str Enter key 300 wait-idle with-screen send-keys   # keys, settle, read
fux %2 opts marks capture            # the screen; ⟦selection⟧ marked
fux %2 opts json capture             # the screen and state as JSON
fux %2 opts wait-exit 30 timeout wait   # exited 0; fux then exits with the program's status
fux opts str bash program 300 wait-idle run   # a shell: it marks its commands; prints %3
fux %3 str 'make test' line          # a shell command, typed and waited to its end: done STATUS
fux str agent workspace kill         # at the end: all run started, closed at once
```

- **Start:** `run` starts a program (not a shell) in a tab of its own in
  workspace `agent`. OPTIONS: `str app name` (the tab's), `100 30 size`,
  `str "$PWD" cwd` (a real path), `str K str V env`, and the program: `str 'prog -x a' command`
  (split as a shell splits), or `str PROG program` and `str A arg` per
  argument. A WAIT among them waits from its first output (`str T wait-text`).
  Use the `%2` it prints: names repeat, and each `fux` is a fresh program
  (a `constant` lasts only within one). The pane stays after the program ends.
- **Wait:** `str T wait-text` (T shows anew, even scrolled away; not a typed
  line's echo), `str T wait-gone`, `MS wait-idle` (no change for MS; after keys,
  status 5 if they changed nothing), `wait-change`, `wait-done` (a shell's
  command ended: `done STATUS`), `wait-exit`, `wait-bell`; `SECS timeout`
  (60), `with-screen`, `marks`; several: whichever comes first. On `keys`
  it is armed before they go in. A
  program that exits ends any other wait with status 3: if it may, wait for
  `wait-exit`. A timeout (124) says what the pane is doing. A tab or
  workspace waits on all its panes.
- **Read:** READ also takes `main-screen` (what was printed before a program
  took the screen), `alt-screen`, `last-output` (a shell command's output).
- **Keys:** names are tmux's (`Enter`, `Escape`, `Up`, `C-c`, `F5`, `BSpace`;
  a typo is refused). Inside `keys … send-keys` text is `text`, not `type`.
  Keys print nothing: add a WAIT, or capture after.
  Bundled: `%2 str T type`, `%2 str Enter press` (each takes its pane), and
  `%3 str LINE line`, for a shell only: a program never ends a command, so
  give it `type`, `press` and a `wait-text`.
- **Mouse:** `%2 str click ROW COL mouse send-mouse` (`wheel-up`, …), from 0.
- **Sizes:** `%2 tab 40 12 resize`; `%2 50 percent resize-x`; `%2 opts
  horizontal str PROG program split` (or `vertical`).
- **Stop:** `%2 str TERM signal` (or `KILL`); `%2 kill` closes one pane, and
  the panes left in its tab grow, which their programs see; `str agent
  workspace kill` closes all at once; `%2 opts respawn` starts it again.
- **Status:** 0 done; 1 failed, `fux: CODE: MESSAGE` on stderr, with how the
  word is used; a wait's own: 3 exited first, 4 closed, 5 no change, 6
  changed, 124 timed out. Words before a failing one have already acted.
  `fux ls` (`ls json`) lists everything.

## A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```sh
# test.fux
opts str tests name str bash program 300 wait-idle run constant T
T str 'cargo test' line drop
T opts all-history last-output capture
```

`define NAME … end` makes a word, `if … else … then` and `begin … until`
decide and repeat, `eq lt gt not add sub` compute; `fux help` has the rest.

## Shells

Each shell fux starts reads your own startup files, then marks its commands
(OSC 133): `wait-done`, `last-output`, `command_running` and `last_status`
then just work. Your prompt, `PROMPT_COMMAND`, hooks and history behave as
before. `str shell-marks str off set` turns this off. A shell fux does not
know, or one started with a script or `-c`, is left alone (bash with
`--norc`, or a login bash, is marked through `PS0` and `PROMPT_COMMAND`); it
can still mark itself (`printf '\e]133;C\a'` as a command's output begins,
`printf '\e]133;D;%s\a' $?` as it ends). Without marks, wait for text only
the output has, or for `wait-idle`.

## Sizes

A tab no client shows is laid out on its `size` (an option of `run`,
`new-tab` and `new-workspace`); a program starts at its size. `TAB 40 12
resize` changes it later (a pane's: its tab; `resize-auto` goes back to a
client's screen). `PANE 95 percent resize-x` sizes a pane within its tab. A
size that cannot be had (over 500 a side, too small for a tab's panes, a
split with no room left) is refused, and nothing changes.

## Which server

`FUX_SOCKET` chooses the server; `str NAME server` (a server of your own,
`NAME.sock` in fux's directory) or `str PATH socket` first chooses another; `start` starts one and returns
once it answers; `status` says whether one answers. Inside a person's pane,
`here` is that pane, but only on that pane's own server (`FUX_SERVER` says
which): on yours it is not acted on. A person can give you `str agent str
PATH grant`: a socket that reaches workspace `agent` alone, which they can
watch in their own server (`str PATH socket` first). `kill-server` from
inside the server you would stop is refused; `force-kill-server` stops it.

## More

- `PANE opts str PASS wait-text str FAIL wait-text wait` waits for either.
- `opts str FILE log-to` on `run` keeps everything a program writes
  (`PANE str FILE start-log` from any time). `PANE str TEXT notify` shows a
  notice to people watching.
- `dump` prints what there is as a fuxth program that makes it again; run it
  with `fux FILE`. A crashed server's next run brings its panes back,
  programs waiting for `respawn`.
- `100 30 headless` is a client with no terminal (it prints its id, `c2`);
  `str c2 client keys … send-keys` types at it as a person would, its
  bindings included, and `str c2 client capture-client` shows what it shows:
  how to drive fux's own menus and prompts.
- Rows and columns count from 0 everywhere.
- Stop what you start by its pid (`kill $!`), not with `pkill -f`.
- fux takes no screenshots: a capture is text, styles and state. It never
  reads the clipboard.
