# Driving programs with fux

(This is `fux help agents`; `fux help COMMAND` explains any command, its
flags and an example.)

How a script, a test or an AI agent drives terminal programs through fux:
start one, send it keys, wait for it, read its screen, clean up. Every step
is one `fux` command, and none touches the tabs a person is using. No
helper script, `sleep`, nested tmux or screenshot is needed; if you find
yourself writing one, something is missing from fux.

## The recipe

```sh
fux run -n ck --size 120x40 --wait-text posts -- ~/ck/target/release/ck
P=ck                                                    # -t NAME finds its pane
fux send-keys -t $P j j Enter --wait-idle 300 --print   # keys, settle, screen
fux capture-pane -t $P --json --styles                  # the screen, styled
fux send-keys -t $P q && fux wait -t $P --exit          # prints: exited 0
fux kill-workspace -t agent                             # all run started
```

- **`fux run [--size COLSxROWS] -- PROGRAM ARGS…`** starts the program
  itself, not a shell, in a tab of its own in workspace `agent` (`-t WS`
  for another), which no client is moved to. `--cwd DIR` and `--env K=V`
  set where and with what; `-n NAME` names its tab, and `-t NAME` then
  finds its pane in any command. `--log FILE` keeps everything it writes,
  past the history limit. `--wait-text TEXT` (or `--wait-idle MS`,
  `--wait-exit`, `--wait-bell`) waits from the program's very first output,
  so a line it prints at once and scrolls away is not missed; it prints the
  pane's id, then the line. When the program exits its pane stays, showing
  its last screen: `fux wait --exit` prints `exited STATUS` (or
  `killed by SIGSEGV`), `fux ls` shows it, and `send-keys` to it fails
  rather than typing somewhere else. A program that cannot start fails
  `run`, saying why.
- **`fux respawn -t %N`** starts it again in the same pane: same words,
  place and variables (`--env K=V` changes one, `-- PROGRAM…` replaces
  it). The pane keeps its id; `-k` ends the program first if it still runs.
- **`fux wait -t %N …`** answers as soon as the pane:
  - shows `--text TEXT`, on the screen or in a line that scrolled away
    since the wait began, and prints that line (`--text` again: any of
    several);
  - stops showing `--gone TEXT`;
  - shows no change for `--idle MS` (a program that redraws the same
    screen is idle);
  - `--change`s at all (`--since N`: past revision N, from `--json`);
  - its program `--exit`s, or a shell command is `--done`, or it rings the
    `--bell` (below).

  A program that exits first ends any wait, with status 3, so a crash is
  not waited out. `--timeout SECS` (60 by default) gives up with status
  124. `--print` prints the screen as it ends; `--json` says what happened
  (`{"met":"text","line":"…","status":0}`). `-t` again watches several
  panes, and the answer starts with the pane that gave it. The server
  checks as output arrives, so a wait costs nothing while it waits.
  - A busy Claude Code (or any program with a spinner or a clock) changes
    its screen all the time; `--idle 5000` waits until it has shown no
    change for five seconds.
- **`fux send-keys -t %N KEYS…`** sends key names (`Enter`, `Escape`,
  `C-c`, `M-x`, `S-Tab`, `Up`, `F5`, `BSpace`; `fux list-keys` lists them)
  as the program asked for keys, and every other word as text.
  `--text TEXT` is text however it looks; `--paste TEXT` is a paste,
  bracketed if the program asked for that. `-N 40 BSpace` repeats.
  - A lone `Escape`, and text, are each read by the program on their own:
    `Escape Escape` is two Escapes, not Alt-Escape, and in
    `--text "message" Enter` the Enter submits rather than joining the text
    as a pasted newline.
  - A word that looks like a key name mistyped (`Entr`, `C-Ent`) is
    refused, not typed as text; `-l` or `--text` sends it as text.
  - Every wait condition goes on send-keys too: `--wait-text TEXT`,
    `--wait-gone`, `--wait-idle MS`, `--wait-change`, `--wait-exit`,
    `--wait-done`, `--wait-bell`, with `--timeout`, `--print` and
    `--json`. The wait is armed before the keys go in, so an answer that
    comes at once is not missed, and `--wait-text` waits for the text to
    appear anew, not for a copy already on the screen. `--wait-idle` exits
    5 if the keys changed nothing.
- **`fux send-mouse -t %N click|right|middle|press|release|wheel-up|wheel-down|move ROW COL`**
  sends a mouse event at a cell, encoded as the program asked; it fails if
  the program did not ask for mouse reports.
- **`fux capture-pane -t %N`** prints the screen with trailing blanks and
  blank rows trimmed. `-J` joins rows the terminal wrapped; `--rows A-B`
  and `--cols A-B` read a region; `-S -LINES` adds history (`-S all`, all
  of it); `--full` keeps every cell. `--main` reads the main screen while a
  program holds the full screen: what was printed before it took over.
  `--last-output` is the last shell command's output alone. `--styles` adds colours and attributes, as SGR escapes or,
  with `--json`, as runs per line, so a selection or a cursor drawn in
  reverse video is visible.
- **`--json`**, on `capture-pane` and `ls`, says what each pane's program
  is doing:
  - `program` (its foreground process), `argv` and `cwd`;
  - `exited` (the status) and `signal` (what killed it);
  - `idle_ms` (how long its screen has shown no change), and `revision`,
    which grows with every change;
  - `running` and `last_status`, from the shell's marks (below);
  - `bells` and `notification`, the last desktop notification;
  - `cursor_visible`, `alt_screen`, `mouse`, `bracketed_paste`,
    `copy_mode` and `history_lines`.

## Shell commands

A shell that marks its commands (OSC 133, as fish, zsh and bash
integrations do) tells fux when one ends and how: `fux wait --done`
(`send-keys … --wait-done`) prints `done STATUS`, `capture-pane
--last-output` prints that command's output alone, and `--json` has
`running` and `last_status`. A bash without them gets them from two
variables:

```sh
C='\e]133;C\a' D='printf "\e]133;D;%s\a" "$?"'   # output starts; ended, with $?
P=$(fux run --env "PS0=$C" --env "PROMPT_COMMAND=$D" -- bash --norc -i)
fux send-keys -t $P --text 'make test' Enter --wait-done --timeout 600
fux capture-pane -t $P --last-output
```

Without marks, `--wait-idle` after the command, or `--wait-text` for a
line it prints at the end, is the fallback.

## When a program needs you

A program asking for attention rings the bell or sends a desktop
notification (OSC 9, 777 or 99), as Claude Code and many agents do when
they finish or need permission. `fux wait --bell` answers then (printing
the notification's text, if any), and `--json` counts `bells` and keeps
the last `notification`. `fux notify [-t TARGET] TEXT` is the other way:
a notice on the screens of the clients showing TARGET, for an agent that
works out of sight to say so.

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
to FILE, and detaches on SIGTERM. `fux resize -c c2 80x24` resizes it,
`fux capture-client -c c2` shows what it shows, and
`fux send-keys -c c2 C-b c` types at it, as a person at its terminal would:
its bindings, menus, prompts and copy mode take the keys, which is how to
drive fux's own screens.

## Ids and the rest

- `split`, `new-tab`, `new-workspace` and `run` print the ids they make
  (`new-workspace`: `+2 @3 %4`), or with `--json` where the new pane is:
  `{"workspace":"+2","tab":"@3","pane":"%4"}`. No need to read `fux ls`.
- `fux ls --json` lists everything; `fux help` lists every command, and a
  tmux name (`list-panes`, `new-window`) says what fux calls it.
- Exit status: 0 done, 1 failed (the reason on stderr), 2 a usage error;
  a wait also exits 3 when the program exited first, 4 when the pane
  closed, 5 when `--wait-idle` saw no change, and 124 on a timeout.
- Stop what you start by the pid you started (`fux attach --headless … &`
  then `kill $!`), not with `pkill -f`: a pattern also matches the shell
  line that runs it, and kills that.

## What fux does not do

- It takes no screenshots: a capture is text, styles and state. A program
  that draws sixel pictures does so only for a terminal that shows them
  (see the README); a pane no such terminal shows is told none, so its
  programs draw in text. `--json` lists `pictures` and where they are.
- It never reads the clipboard; a program's copy (OSC 52) goes to fux's
  paste buffers and to the terminals showing the pane.
- It has no mouse actions of its own.
