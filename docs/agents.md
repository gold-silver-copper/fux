# Driving programs with fux

(`fux help agents`. This first screen is enough for most tasks; `fux help
COMMAND` has every flag.) Each step is one command, none touches a person's
tabs, and no `sleep`, polling loop or helper script is needed.

```sh
fux -L mine server --background                 # a private server: -L mine on each
fux run -n app --size 100x30 --wait-text Ready -- ./app   # start; waits for Ready
fux capture-pane -t app --marks                 # the screen; ⟦selection⟧ marked
fux send-keys -t app j j Enter --wait-idle 300 --print    # keys, settle, read
fux run -n sh --wait-idle 300 -- bash           # a shell: it marks its commands
fux send-keys -t sh --text 'make test' Enter --wait-done  # a command, to its end
fux wait -t app --exit                          # prints: exited 0
fux ls -t app --get exited                      # one value: 0
fux kill-workspace -t agent                     # everything run started
fux -L mine kill-server --force                 # the private server, at the end
```

- **Start:** `run -n NAME [--size COLSxROWS] [--cwd DIR] [--env K=V] -- PROG…`
  starts the program itself (not a shell), here, out of every client's way;
  its flags go before `--`. `-t NAME` then finds it in every command (a name
  in use is refused). The pane stays after the program exits.
- **Wait:** `run`, `respawn`, `send-keys`, `send-mouse` and `signal` take
  `--wait-…` flags, armed before they act so nothing is missed; `fux wait -t
  NAME` takes the same without `wait-` (`--text`, `--gone`, `--idle`,
  `--change`, `--exit`, `--done`, `--bell`); `fux events --json` streams what
  happens instead (made, closed, exited, done, bell), a line each:
  - `--wait-text TEXT`: until TEXT shows on a line that changed since (also
    one that scrolled away; not the echo of a typed line, nor a line drawn
    again the same); prints its line. `fux wait --text A
    --text B -t x -t y`: any of several, in any of several panes.
  - `--wait-idle MS`: no change for MS (a spinner is change); after keys or a
    click, status 5 means it changed nothing. `--wait-change`: any change.
  - `--wait-done`: the shell is back at its prompt after a command ends;
    prints `done STATUS` (after Ctrl-C too).
  - `--wait-exit`: the program exits; prints `exited STATUS`, exits with it.
  - `--wait-gone TEXT`; `--wait-bell` (a bell or notification: it needs you).
  - `--timeout SECS` (60), `--print` (the screen after), `--json`.
  A crash ends a wait at once (status 3, its last lines printed); a timeout
  (124) says what the pane is doing ("no change for 58s; last line: `> `").
- **Read:** `capture-pane -t NAME` prints the screen, trimmed. `--marks` shows
  highlights (reverse video, or a background other than the screen's: a
  menu's selection) as `⟦beta⟧` (a menu may start on any
  item); `--styles` adds colours (with `--json`, as runs). `-S all` adds
  history; `--last-output` is the last shell command's output; `--alt` is a
  full-screen program's last screen after it exits; `--main` what was
  printed before it took the screen. `--json` adds state: `exited`,
  `signal`, `cursor_visible`, `alt_screen`, `last_status`, `bells`, …
- **Keys:** tmux's names (`Enter`, `Escape`, `C-c`, `Up`, `F5`, `S-Tab`,
  `BSpace`; `fux list-keys`); other words, and `--text TEXT`, are typed; a
  misspelled name (`Entr`) is refused; `-l` types every word. `--paste TEXT`
  pastes (`--paste-file F`, `--paste -`: stdin); `-N 5` repeats; `-t` again
  sends to several. Flags go before the keys; words after `--` are typed.
  `--if-revision N` (from `--json`) sends nothing, status 6, if it changed.
- **Mouse:** `send-mouse -t NAME click|wheel-up|wheel-down… ROW COL`, from 0.
- **Sizes:** `resize -t NAME 40x12` (its tab); `resize-pane -t %N -x 50%`;
  `split -h|-v -t %N [--cwd DIR] -- PROG` (one word is typed as a line).
- **Stop:** `send-keys C-c`; `signal -t NAME TERM|KILL` for one that ignores
  it; `kill-pane -t NAME`, `kill-tab -t NAME`, `kill-workspace -t agent`
  (each needs -t, and says what it closed); `respawn -t NAME` starts it again.
- `fux ls [--json]` lists workspaces, tabs and panes.
- **Status:** 0 done, 1 failed (why on stderr; with `--json`, JSON with a
  code), 2 usage, 6 changed (`--if-revision`); a wait also 3 (the program
  exited first), 4 (pane closed), 5 (no change), 124 (timed out).

## Shells

Each shell fux starts reads your own startup files, then marks its commands
(OSC 133): `--wait-done`, `--last-output`, `command_running` and `last_status` then
just work. Your prompt, `PROMPT_COMMAND`, hooks and history behave as
before. `set shell-marks off` in the config turns this off. A shell fux does
not know, or one started with a script or `-c`, is left alone (bash with
`--norc`, or a login bash, is marked through `PS0` and `PROMPT_COMMAND`); it
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
cannot be had (over 500 a side, or too small for a tab's panes, or a split
with no room left) is refused, and nothing changes.

## Which server

`fux -L NAME` (a server of your own, `NAME.sock` in fux's directory) or `-S
PATH` before any command chooses the server; `fux -L NAME server
--background` starts one and returns once it answers; `fux status` says
whether one answers. Inside a person's pane, `FUX_PANE` is that pane, but only
on that pane's own server (`FUX_SERVER` says which): on yours it is not acted
on. A `FUX_PANE` set by hand, with no `FUX_SERVER`, is taken as given.
A person can give you
`fux grant -t agent --socket PATH`: a socket that reaches workspace `agent`
alone, which they can watch in their own server (`-S PATH` on each command).
`kill-server` from inside the server you would stop needs `--force`.

## More

- `fux wait -t %3 -t %4 --text PASS --text FAIL` watches several panes for
  any of several texts; the answer starts with the pane. `--since N` with
  `--change` answers at once for a change past revision N (`--json`).
- `run --log FILE` keeps everything a program writes (`log -t NAME FILE`
  from any time). `notify -t TARGET TEXT` shows a notice to people watching.
- `fux layout FILE` makes several panes at once from fux command lines, all
  or nothing (`fux help layout`); `fux dump-layout` writes one. A crashed
  server's next run brings its panes back, programs waiting for `respawn`.
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
