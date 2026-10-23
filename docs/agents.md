# Driving programs with fux

(`fux help-agents`: this screen, enough for most work. A topic at length:
`fux help agents TOPIC`, below; `fux help` lists every word, `fux help WORD`
explains one.)

A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed. The server is
`FUX_SOCKET`'s; `fux start` starts one if none runs. **The word that acts
comes last:** what it takes (keys, a wait, what to read) goes on the pane
before it, and `json` after it. Only `run`, `new-tab` and `new-workspace`
start from `opts`, and only `run` takes a program: the other two start a
shell (`str 'CMD' typed` types a line into it).

**Act, then read the answer.** Every action waits until the pane settles
(30s at most), then answers with its state and what came since your input:
you never sleep or poll.

```sh
fux opts str web name 100 30 size str ./app program str --fast arg run  # start: answers %2 reading, then its output
fux opts str 'cargo test' command run wait-exit wait   # start, and wait for its end
fux %2 str y text str Enter key send-keys      # keys (str T paste send-keys pastes): answers as run does
fux %3 str 'make test' line                    # a shell command: %3 reading · done 0, its output
fux %2 wait                                    # settle again; `%2 300 timeout wait` waits longer
fux %2 str 'listening on' wait-text wait       # wait for a text; also wait-gone, wait-bell, wait-exit
fux %2 all-history capture                     # read the screen; also other-screen, last-output, styles
fux %2 capture json                            # as JSON; `%2 str state get` is one field
fux %2 str C-c key send-keys                   # a key: Enter, C-c, Up, F5 (`fux key-names`)
fux %2 str click 3 10 mouse send-mouse         # a click at row 3, column 10, from 0
fux %2 tab 80 24 resize; fux %2 50 percent resize-x; fux %2 horizontal split
fux %2 str TERM signal; fux %2 respawn         # stop it; start it again
fux str web pane                               # a pane by its tab's name
fux ls                                         # what there is: workspaces, tabs, panes
fux str agent workspace kill                   # at the end: everything run started
```

```
%2 reading                 it waits for you: blocked reading its terminal, every key read
%2 reading · done 0        a shell command ended with status 0, and the shell reads again
%2 reading · no change     it read your keys and drew nothing: do not send them again
%2 waiting                 quiet 1s, but it may go on alone (the network, a timer)
%2 busy                    still at work at the time limit (status 124): `%2 wait` waits on
%2 exited 3                the program ended; what it printed since your input follows
%2 reading · 80 lines above    a capture left out history: `all-history` reads it
%2 reading · earlier lines gone  what came before is gone: a full screen keeps none
```

- **Status:** 0 done; 1 failed (`fux: CODE: MESSAGE` on stderr); 3 the
  program exited before the wait; 4 the pane closed; 5 the shell command
  `wait-done` waited on ended first; 7 it reads its terminal and the text
  has not come; 124 still at work at the limit, not failed.
- **Clean up:** `fux str agent workspace kill`; stop what you start by its
  pid, never `pkill -f`.

Topics (`fux help agents TOPIC`; `all` for the whole guide): `answers`,
`keys`, `waits`, `read`, `shells`, `programs`, `servers`.

## answers: What an answer holds

The first line is the pane and its state, then notes, each `· NOTE`; then
what came since your input: from the line typed on if it changed (its
prompt and your echo too), else from the first line that changed; on a
full-screen program, its whole screen; a program that left its full
screen, what it printed after; one that ended on it (an editor quit), its
last screen, then what came after.

The notes: `done N` (a shell command ended), `no change` (keys read,
nothing drawn), `command running` (a marked shell command runs on),
`redraws on its own` (a clock, `top`: what it draws later may differ),
`highlight ROW,COL-END "TEXT"` (up to three runs the input highlighted
anew: a selection, a menu's choice), `no status: nothing here marks its
commands` (a shell fux did not start read a line: no `done` comes), `it
exited before reading C-q, which went nowhere` (keys sent after the one
that quit it), `N bytes unread` (input fux still holds), and, after a wait
for a text or a bell, `met "TEXT"` with the line that held it. JSON
(`json`) has each as a field: `fux help json`.

Every answer stands alone, so a chain's last word answers in full (`fux
opts str 'cargo test' command run wait-exit wait`). Keys sent to a program
that exited are refused (`not_running`).

Echo is no output. fux records which input each piece of output followed
and tells echo from output by the shell's marks, the terminal's echo rules
and what was typed: a text made only of characters you typed on the line,
before Enter, is taken as their echo; after Enter, a line printed again
exactly as typed is output. A full-screen program's screen is all output.

## keys: Keys, the mouse and stopping

- **Keys** go between the pane and `send-keys`: `str T text`, `str NAME
  key` (tmux's names: `Enter`, `Escape`, `Tab`, `BSpace`, `Up`, `PageDown`,
  `F5`, `C-c`, `M-s`, `S-Tab`; `Esc`, `Backspace`, `PgUp` too; a typo is
  refused, naming the closest), `str T paste` (bracketed if the program
  asked), `str FILE paste-file`, `str HEX bytes` (`1b5b41`), `MS pause`
  between two inputs (1 to 10000 ms: input that arrives split), `N times`
  (repeats the input just before it). 900 KiB at most in all.
- Each input goes in once the program took the one before, so what it draws
  follows the key it answers; a full-screen program, or one reading lines,
  gets them at once. The answer comes once the last went in and the pane
  settled.
- **Mouse:** `%2 str click ROW COL mouse send-mouse` (also `right`,
  `middle`, `press`, `release`, `wheel-up`, `wheel-down`, `move`), cells
  from 0 as `wrapped capture` counts them; on a double-width row, click at
  2×COL. A program's own "row 22" is usually counted from 1.
- **Stop:** `%2 str TERM signal` (`INT`, `KILL`, …) answers once the
  program exits or settles; `%2 kill` closes the pane; `%2 respawn` starts
  its program again (`kill-first` ends it first).

## waits: Time, and waiting for a text

- An action, a `capture` and a `wait` wait 30s at most (the server's
  `wait-seconds`); `SECS timeout` on the pane changes it for one. `0
  timeout` answers at once. A `wait` on a tab or workspace waits for each
  pane, a line each, the first failing status naming the whole.
- Busy at the limit is status 124, not a failure: `%2 wait` waits on. A
  shell command of unknown length: `%2 wait-done wait` waits as long as it
  runs; with a text, `%2 str 'build ok' wait-text wait-done wait` fails
  with status 5 if the command ends first.
- Wait words go on the pane before `wait`, or before the action they
  follow: `str T wait-text`, `str T wait-gone` (after `run`, the text must
  show first), `wait-exit`, `wait-bell`; several, whichever comes first.
- A text matches case ignored, blanks as one, from a word's start (`READY`
  is not in `NOT_READY`); `^T` is a line's first word, `T$` its last. It
  matches what the program printed since your input, lines scrolled away and
  text drawn over too, never the echo of what you typed. Met, the answer is
  the state line, `· met "T"`, then the line that held it.
- **Status 7:** the program is blocked reading its terminal and the text
  has not come (or gone): send it what it waits for. A program that may go
  on alone (`waiting`) gets no 7: the wait runs to its limit.
- A wait that fails (3, 4, 5, 124) answers first, with the pane's last
  lines, then says why on stderr. A met `wait-exit` is status 0; the program's own status
  is in its state (`exited 3`) and in `str exited get`.

## read: Reading the screen

`PANE READ capture`, READ any of: `100 history`, `all-history`,
`other-screen` (the screen not shown: under a full-screen program, what
was printed before it; after one, the last screen it drew), `last-output`
(the last shell command's output, or the running one's so far; with no
command marked, the screen), `wrapped` (each screen row a line; else
a line wrapped at the pane's width is joined whole), `0 4 rows`, `0 39
cols`, `full`, `marks` (highlights as `⟦this⟧`), `styles`; then `json` for
the screen and the pane's state (`cursor` is `[ROW,COL]`). Rows and
columns count from 0.

A capture's state line says what it leaves out: `· 80 lines above`, the
history it did not read, and `· earlier lines gone`, lines before those
kept are gone (a full-screen program's screen keeps no history; history
past `history-bytes`, or cleared; a last output whose start is gone).
Nothing said: you have it all. JSON says the same in `above` and `gone`
(a program's capture, with no state line, only there). A full-screen
program that redraws its screen keeps only what it shows: run an agent's
TUI inline where it can (e.g. `codex --no-alt-screen`), and `all-history`
reads its whole answer.

## shells: Shells and their marks

`fux opts str agent in new-tab` gives a shell. A bash, zsh or fish fux
starts marks its commands (OSC 133, with a nonce output cannot fake):
`line` answers `· done STATUS` once the command ends, and `last-output`
reads its output alone; a command that waits for keys (`read`, `sudo`, a
pager) answers `reading`, its prompt shown. `str shell-marks str off set`
turns marks off for shells started after. A shell fux did not start (`str
sh program`) gives no status: `CMD; echo "status $?"` prints it.

A program that marks commands of its own (a relay like `koh connect`, a
nested multiplexer) marks with `;nonce=$FUX_MARK_NONCE` among its OSC 133
options; a relay may report its remote program's state with `ESC ] 133 ;
S ; activity=STATE ; nonce=N BEL` (`reading`, `polling`, `waiting`,
`busy`, `idle`, `unknown`, or `ended`). Plain `ssh` reports nothing: fux
sees the client, `waiting` once quiet. Wait for the text you expect, or run
a remote command in a pane of its own (`fux opts str 'ssh -n HOST make'
command run`, then `%2 wait-exit wait`).

## programs: A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```sh
# test.fux
opts str tests name str bash program run let T
T str 'cargo test' line drop
T all-history last-output capture
```

`X let NAME` names a value, `define NAME … end` makes a word, `if … else …
then` and `begin … until` decide and repeat, `list … end` gathers what its
body leaves, `SEQ each … end` runs once per element. An action leaves its
answer: printed, it is the lines above; given to a word that wants a pane,
it is its pane; `string` makes it text (`%2 screen string str error
contains`). The standard library, written in fuxth, does the rest (`fux
help` lists it). A mistake every run would make (too few values, a string
given to `add` or `if`, a request nothing acts on: its error names the word
to end it with) fails before any word acts; words before a failing one have already acted otherwise. A run exits
with the status of the first answer it leaves that has one.

## servers: Servers, sizes and more

- `str NAME server` (`NAME.sock` in fux's directory) or `str PATH socket`
  first chooses another server; `start` starts one, `status` says whether
  one answers. Inside a person's pane, `here` is that pane. A person can
  give you `str agent str PATH grant`: a socket that reaches workspace
  `agent` alone. `kill-server` from inside its own server is refused;
  `force-kill-server` stops it.
- A server that did not stop cleanly leaves its layout in
  `$XDG_STATE_HOME/fux`: the next server on that socket brings it back, each
  program waiting for `respawn`. `kill-server` forgets it.
- In a sandbox that refuses Unix sockets, client and server meet through a
  FIFO beside the socket (`NAME.door`).
- A tab no client shows is laid out on its `size`; `TAB 40 12 resize`
  changes it, `resize-auto` gives it back to a client's screen, `PANE 95
  percent resize-x` sizes a pane in its tab. A size that cannot be had is
  refused, and nothing changes.
- `opts str FILE log-to` keeps what a program writes; `dump` prints the
  layout as a program that makes it again; `100 30 headless` is a client
  with no terminal (`str c2 client … send-keys` types at it,
  `capture-client` shows it): how to drive fux's own menus.
