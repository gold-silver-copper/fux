# Driving programs with fux

(`fux help-agents`; `fux help` lists every word, `fux str WORD explain` one.)
A fux command line is a fuxth program: words run left to right on a stack,
each taking what the words before it left. `str WORD` pushes WORD as text;
`%3`, `@2`, `+1` are a pane, tab, workspace. What is left is printed, a value
a line. Each step is one command; none touches a person's tabs; no `sleep`.
The server is `FUX_SOCKET`'s, or the default; start one with `fux start`.

```sh
fux opts str app name 100 30 size str './app --fast' command str Ready wait-text run   # start; waits for Ready
fux str app pane opts marks capture                # the screen; ⟦selection⟧ marked (or %3 opts …)
fux str app pane keys str j key str Enter key 300 wait-idle with-screen send-keys  # keys, settle, read
fux opts str sh name str bash program 300 wait-idle run   # a shell: it marks its commands
fux str sh pane str 'make test' line               # a shell command, to its end: done STATUS
fux str app pane opts wait-exit wait               # exited 0, and exits with the program's status
fux str app pane str exited get                    # one value: 0
fux str app pane opts json capture                 # the screen and state as JSON
fux str agent workspace kill                       # everything run started
```

- **Start:** `opts … run` starts a program (not a shell) in a tab of its own
  in workspace `agent`, out of every client's way, and leaves its pane
  (`%3`). Options: `str NAME name`, `COLS ROWS size`, `str DIR cwd`, `str K
  str V env`, and the program: `str 'prog a b' command` (split as a shell
  splits), or `str PROG program` then `str A arg` per argument. `str NAME
  pane` finds it later. The pane stays after the program exits.
- **Wait:** a condition on `run`'s options, on `keys` (`send-keys`), on a
  `mouse` event, or on `opts` for `PANE opts … wait`; armed before it acts:
  `str T wait-text` (T shows anew, even scrolled away; not a typed line's
  echo), `str T wait-gone`, `MS wait-idle` (no change for MS; after keys,
  status 5 if they changed nothing), `wait-change`, `wait-done` (a shell's
  command ended: `done STATUS`), `wait-exit` (`exited STATUS`; fux exits
  with that status), `wait-bell`. `SECS timeout` (60), `with-screen`. A crash
  ends a wait (status 3, last lines shown); a timeout (124) says what the
  pane is doing. A tab or workspace waits on all its panes.
- **Read:** `PANE opts capture` (`PANE screen`), trimmed; options `marks`
  (highlights, a menu's selection, as `⟦beta⟧`), `styles`, `100 history`,
  `all-history`, `last-output`, `alt-screen`, `main-screen`, `json` (state
  too: `exited`, `signal`, `cursor_visible`, `alt_screen`, …).
- **Keys:** `PANE keys … send-keys`: `str TEXT text`, `str C-c key` (tmux's
  names: `Enter`, `Escape`, `Up`, `F5`, `BSpace`; a typo is refused), `str
  TEXT paste`, `5 times`, `N if-revision` (status 6 if it changed). They
  print nothing; add a wait, or capture after. Bundled: `PANE str T type`,
  `PANE str Enter press`, `PANE str LINE line` (for a shell: typed, Enter,
  waited to its end).
- **Mouse:** `PANE str click ROW COL mouse send-mouse` (`wheel-up`, …), from 0.
- **Sizes:** `PANE tab COLS ROWS resize`; `PANE 50 percent resize-x`; `PANE
  opts horizontal str PROG program split` (or `vertical`).
- **Stop:** `PANE str TERM signal` (or `KILL`); `PANE kill`; `PANE opts
  respawn` starts it again.
- **Status:** 0 done; 1 failed, `fux: CODE: MESSAGE` on stderr, which says
  how the word is used; a wait's own: 3 exited first, 4 closed, 5 no change,
  6 changed, 124 timed out. `fux ls` (`ls json`) lists everything.

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
