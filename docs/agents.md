# Driving programs with fux

How a script, a test or an AI agent drives terminal programs through fux:
start one, send it keys, wait for it, read its screen, clean up. Every step
is one `fux` command, and none touches the tabs a person is using. No
helper script, `sleep`, nested tmux or screenshot is needed; if you find
yourself writing one, something is missing from fux.

## The recipe

```sh
P=$(fux run --size 120x40 -- ~/ck/target/release/ck)   # start it; prints %N
fux wait -t $P --text "posts" --timeout 20              # until it has loaded
fux send-keys -t $P j j Enter --wait-idle 300 --print   # keys, settle, screen
fux capture-pane -t $P --json --styles                  # the screen, styled
fux send-keys -t $P q && fux wait -t $P --exit          # prints: exited 0
fux kill-workspace -t agent                             # all run started
```

- **`fux run [--size COLSxROWS] -- PROGRAM ARGS…`** starts the program
  itself, not a shell, in a tab of its own in workspace `agent` (`-t WS`
  for another), which no client is moved to. `--cwd DIR` and `--env K=V`
  set where and with what. When the program exits its pane stays, showing
  its last screen: `fux wait --exit` prints `exited STATUS`, `fux ls`
  shows `exited STATUS`, and `send-keys` to it fails rather than typing
  somewhere else. A program that cannot start fails `run`, saying why.
- **`fux wait -t %N …`** answers as soon as the pane shows `--text TEXT`
  (give `--text` again to wait for any of several), stops showing
  `--gone TEXT`, writes nothing for `--idle MS`, `--change`s at all, or its
  program `--exit`s. It exits 1 after `--timeout SECS` (60 by default,
  under what a tool waits for one command), saying so. `--print` prints the
  screen as it ends, timeout or not. The server checks as output arrives,
  so a wait costs nothing while it waits.
  - A busy Claude Code (or any program with a spinner) writes all the time;
    `--idle 5000` waits until it has been quiet for five seconds.
- **`fux send-keys -t %N KEYS…`** sends key names (`Enter`, `Escape`,
  `C-c`, `M-x`, `S-Tab`, `Up`, `F5`, `BSpace`; `fux list-keys` lists them)
  as the program asked for keys, and every other word as text.
  `--text TEXT` is text however it looks; `--paste TEXT` is a paste,
  bracketed if the program asked for that. `-N 40 BSpace` repeats.
  - A lone `Escape`, and text, are each read by the program on their own:
    `Escape Escape` is two Escapes, not Alt-Escape, and in
    `--text "message" Enter` the Enter submits rather than joining the text
    as a pasted newline.
  - `--wait-idle MS [--print]` waits after sending, as `wait --idle` does:
    press, settle and read in one call.
- **`fux send-mouse -t %N click|right|middle|press|release|wheel-up|wheel-down|move ROW COL`**
  sends a mouse event at a cell, encoded as the program asked; it fails if
  the program did not ask for mouse reports.
- **`fux capture-pane -t %N`** prints the screen with trailing blanks and
  blank rows trimmed. `-J` joins rows the terminal wrapped; `--rows A-B`
  and `--cols A-B` read a region; `-S -LINES` adds history; `--full` keeps
  every cell. `--styles` adds colours and attributes, as SGR escapes or,
  with `--json`, as runs per line, so a selection or a cursor drawn in
  reverse video is visible.
- **`--json`**, on `capture-pane` and `ls`, says what each pane's program
  is doing: `program` (its foreground process), `exited`, `idle_ms`,
  `alt_screen`, `mouse` and `bracketed_paste`.

Rows and columns count from 0 everywhere: `--rows`, `--cols`,
`send-mouse` and the `cursor` in `--json`.

## Sizes

A tab no client shows is laid out on its `--size` (on `run`, `new-tab`
and `new-workspace`), or on any attached client's screen without one. A
program starts at its size, so it never draws for 80x24 and then resizes.

- `fux resize -t @N|+N COLSxROWS` changes it later, to test a narrow
  layout: `fux resize -t +3 40x12`. `auto` goes back to a client's screen.
- `fux resize-pane -t %N -x 95%` makes a pane 95% of its tab's width;
  `-x COLS`, `-y ROWS` and `-y N%` work the same. A size the layout cannot
  give is refused, and nothing changes.
- A client that shows a tab lays it out on its own screen, whatever the
  tab's size.

## Several clients, and testing fux itself

`fux attach --headless --size 100x30 [--log FILE]` attaches a client with
no terminal: it prints its id (`c2`), writes everything the server sends it
to FILE, and detaches on SIGTERM. `fux resize -c c2 80x24` resizes it, and
`fux capture-client -c c2` shows what it shows.

## Ids and the rest

- `split`, `new-tab`, `new-workspace` and `run` print the ids they make
  (`new-workspace`: `+2 @3 %4`), or with `--json` where the new pane is:
  `{"workspace":"+2","tab":"@3","pane":"%4"}`. No need to read `fux ls`.
- `fux ls --json` lists everything; `fux help` lists every command, and a
  tmux name (`list-panes`, `new-window`) says what fux calls it.
- Exit status: 0 done, 1 failed (the reason on stderr), 2 a usage error.
- To find your own processes with `pgrep -f` or `pkill -f`, bracket a
  letter (`pgrep -f '[c]k --flag'`), so the pattern does not match the
  shell running it.

## What fux does not do

- It takes no screenshots: a capture is text, styles and state. Images a
  program draws are passed to terminals that show them (see the README).
- It never reads the clipboard; a program's copy (OSC 52) goes to fux's
  paste buffers and to the terminals showing the pane.
- It has no mouse actions of its own.
