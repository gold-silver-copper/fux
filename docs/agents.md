# Driving programs with fux

(This is `fux help agents`. The first screen is enough for most tasks;
`fux help COMMAND` has every flag of a command.)

Start a program, send it keys, wait, read its screen, clean up: each step is
one `fux` command, and none touches the tabs a person uses. No `sleep`,
polling loop or helper script is needed.

```sh
fux run -n app --size 100x30 --wait-text Ready -- ./app   # start; waits for Ready
fux capture-pane -t app --marks                 # the screen; ⟦selection⟧ marked
fux send-keys -t app j j Enter --wait-idle 300 --print    # keys, settle, read
fux send-keys -t app --text 'make test' Enter --wait-done # a shell command, to its end
fux wait -t app --exit                          # prints: exited 0
fux kill-workspace -t agent                     # everything run started
```

- **Name it, then use the name.** `run -n NAME` names the program's tab, and
  `-t NAME` finds it in every command, so ids need not be kept. `run` starts
  the program itself (not a shell), in the directory you are in, in
  workspace `agent`, which no client is moved to.
- **Every action can wait,** armed before it acts so nothing comes too soon
  to be seen: `run`, `respawn`, `send-keys`, `send-mouse` and `signal` take
  the same flags as `wait`:
  - `--wait-text TEXT`: until TEXT appears; prints the line it is on. It is
    found even in a line that scrolled away, and the echo of text you typed
    does not count as output.
  - `--wait-idle MS`: until the screen shows no change for MS (a spinner or
    clock is change). After keys or a click, exit status 5 means it changed
    nothing.
  - `--wait-done`: until a shell command ends; prints `done STATUS`. Shells
    fux starts (bash, zsh, fish) mark their commands with no setup.
  - `--wait-exit`: until the program exits; prints `exited STATUS`.
  - `--wait-gone TEXT`, `--wait-change`, `--wait-bell` (a bell or desktop
    notification: a program that needs you).
  - `--timeout SECS` (60), `--print` (the screen after), `--json`.
- **A wait never waits out a crash:** if the program exits first, the wait
  ends with status 3 and prints its last lines. A timeout is status 124 and
  says what the pane is doing: "still running, no change for 58s; last
  line: `> `".
- **Reading:** `capture-pane` prints the screen, trimmed.
  - `--marks` shows reverse video, which is how menus draw the selection,
    as `⟦beta⟧`. A menu may start on any item: read it before moving.
  - `-S all` adds the history; `--last-output` is the last shell command's
    output alone.
  - `--alt` is the last full screen of a program that has left it or exited
    (a menu's or editor's), and `--main` is what was printed before it took
    the screen.
- **Keys:** names as tmux has them (`Enter`, `Escape`, `C-c`, `Up`, `F5`,
  `S-Tab`, `BSpace`; `fux list-keys`); other words, and `--text TEXT`, are
  typed. A misspelled key name (`Entr`) is refused. `--paste TEXT` pastes.
- **Mouse:** `send-mouse -t app click ROW COL` (from 0; also `right`,
  `wheel-up`, `wheel-down`, `move`…). Add `--wait-text` or `--wait-idle` to
  know whether it landed.
- **Stopping:** `send-keys C-c`; for a program that ignores it,
  `signal -t app TERM` or `KILL`. `kill-pane`, `kill-tab -t NAME` and
  `kill-workspace` print what they closed. `respawn -t app` starts it again
  in the same pane.
- **State:** `capture-pane --json` (and `ls --json`) says `exited`,
  `signal`, `running`, `last_status`, `idle_ms`, `revision`, `bells`,
  `notification`, `cursor_visible`, `alt_screen`, `mouse`, `cwd`, `argv`…
- **Exit status:** 0 done, 1 failed (why on stderr), 2 a usage error; a wait
  also 3 (the program exited first), 4 (the pane closed), 5 (no change) and
  124 (timed out).

## Shells

Each shell fux starts reads your own startup files, then marks its commands
(OSC 133): `--wait-done`, `--last-output`, `running` and `last_status` then
just work. Your prompt, `PROMPT_COMMAND`, hooks and history behave as
before. `set shell-marks off` in the config turns this off. A shell fux does
not know, or one started with a script, `-c` or `--norc`, is left alone; it
can still mark itself (`printf '\e]133;C\a'` as a command's output begins,
`printf '\e]133;D;%s\a' $?` as it ends). Without marks, wait for text only
the output has, or for `--wait-idle`.

`split -- 'cd src && make'` types one argument as a line, as written; with
several words, each is quoted, as a program's arguments. `split` takes
`--cwd` and `--env`, as `run` does.

## Sizes

A tab no client shows is laid out on its `--size` (on `run`, `new-tab` and
`new-workspace`); a program starts at its size. `fux resize -t app 40x12`
changes it later (a pane: its tab; `auto` goes back to a client's screen).
`fux resize-pane -t %N -x 95%` sizes a pane within its tab. A size that
cannot be had is refused, and nothing changes.

## More

- `fux wait -t %3 -t %4 --text PASS --text FAIL` watches several panes for
  any of several texts; the answer starts with the pane. `--since N` with
  `--change` answers at once for a change past revision N (`--json`).
- `run --log FILE` keeps everything a program writes. `notify -t TARGET TEXT`
  shows a notice to people watching.
- `fux attach --headless --size 100x30 [--log FILE]` is a client with no
  terminal (it prints its id, `c2`); `send-keys -c c2` types at it as a
  person would, and `capture-client -c c2` shows what it shows: how to
  drive fux's own menus and prompts.
- `split`, `new-tab`, `new-workspace` and `run` print the ids they make, or
  with `--json` where the new pane is.
- Rows and columns count from 0 everywhere.
- Stop what you start by its pid (`kill $!`), not with `pkill -f`.
- fux takes no screenshots: a capture is text, styles and state. It never
  reads the clipboard.
