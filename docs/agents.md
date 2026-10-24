# Driving programs with fux

(`fux help-agents`: this screen, enough for most work. A topic at length:
`fux help agents TOPIC`, below; `fux help` lists every word, `fux help WORD`
explains one.)

A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed. The server is
`FUX_SOCKET`'s; `fux start` starts one if none runs. **The word that acts
comes last:** what it takes (keys, a version, what to read) goes on the
pane before it, and `json` after it. Only `run`, `new-tab` and `new-workspace`
start from `opts`, and only `run` takes a program: the other two start a
shell (`str 'CMD' typed` types a line into it).

**Every pane has a version, and a wait waits for it to change.** A version
(`0.57.1.3`) counts what only grows: its program's starts (`respawn`), its
screen's revision (any change to the screen or its history), bells and
notifications rung, and shell commands ended (OSC 133 marks). Every action
(`run`, `send-keys`, `send-mouse`, `signal`, `split`, `new-tab`, `respawn`)
acts at once and prints its pane and the version from just before it
acted (a new pane's as it starts): wait from that one, and nothing its
input brings is missed. fux never judges whether a program is done: which
change means done is yours to say (recipes below).

```sh
fux opts str web name 100 30 size str ./app program str --fast arg run  # start: %2 0.1.0.0
fux %2 str y text str Enter key send-keys      # keys: %2 0.57.0.3, its version before them
fux %2 str 0.57.0.3 changed wait               # until it differs: %2 0.61.0.3 screen
fux %2 wait                                    # until it differs from now's
fux %2 str 0.61.0.3 changed 600 timeout wait   # 600s at most (else 30, the server's wait-seconds)
fux %2 all-history capture                     # read the screen; also main-screen, last-output, styles
fux %2 capture json                            # as JSON, with version, exited, reading
fux %2 str version get                         # one field: version, exited, reading, last_status
fux %2 str C-c key send-keys                   # a key: Enter, C-c, Up, F5 (`fux key-names`)
fux %2 str click 3 10 mouse send-mouse         # a click at row 3, column 10, from 0
fux %2 tab 80 24 resize; fux %2 50 percent resize-x; fux %2 horizontal split
fux %2 str TERM signal; fux %2 respawn         # stop it; start it again
fux str web pane                               # a pane by its tab's name
fux ls                                         # what there is: workspaces, tabs, panes
fux str agent workspace kill                   # at the end: everything run started
```

A wait answers the pane, its version and what changed: `%2 0.61.1.4
screen bell done`, `exited 3` or `killed by SIGTERM` once its program
ended, `respawned` once started again (its counts then start afresh). On
a tab or workspace it waits on every pane, answering those that changed.
Status: 0 changed or exited; 1 failed (`fux: CODE: MESSAGE` on stderr); 4
the pane closed (a shell pane closes when its shell exits); 124 nothing
changed in time (`%2 0.61.1.4 timeout`). Clean up with `fux str agent
workspace kill`; stop what you start by its pid, never `pkill -f`.

Topics (`fux help agents TOPIC`; `all` for the whole guide): `recipes`,
`keys`, `read`, `shells`, `programs`, `servers`.

## recipes: Waiting for what you need

Each is a few lines of shell around `changed`; the version a loop holds
is always from before what it waits for, so a change between two calls is
never missed. `fux … | cut -d' ' -f2` takes the version from an answer.

- **A text the reply writes:** count it before, act, and wait until there
  is one more. Text the input itself shows is counted once it shows, so
  it never ends its own wait: here the prompt holds `DONE` too.

  ```sh
  V=$(fux %2 str 'reply DONE when finished' paste send-keys | cut -d' ' -f2)
  until fux %2 all-history capture | grep -q 'reply DONE'; do
    V=$(fux %2 str "$V" changed wait | cut -d' ' -f2)
  done
  n=$(fux %2 all-history capture | grep -c DONE)          # the prompt's own
  V=$(fux %2 str Enter key send-keys | cut -d' ' -f2)
  until [ "$(fux %2 all-history capture | grep -c DONE)" -gt "$n" ]; do
    V=$(fux %2 str "$V" changed 600 timeout wait | cut -d' ' -f2) || break
  done
  ```
- **Quiet for N seconds** (an agent's TUI behind a spinner, done or
  asking you something): wait until no change comes for N seconds.

  ```sh
  V=$(fux %2 str 'fix the test' paste str Enter key send-keys | cut -d' ' -f2)
  while out=$(fux %2 str "$V" changed 5 timeout wait); do
    case $out in *exited*|*killed*) break ;; esac
    V=$(echo "$out" | cut -d' ' -f2)
  done
  ```
- **A shell command's end, with marks** (`shells` says how to turn them
  on): the version's last count grows as one ends.

  ```sh
  V=$(fux %3 str 'make test' line | cut -d' ' -f2)
  until [ "$(fux %3 str version get | cut -d. -f4)" -gt "${V##*.}" ]; do
    fux %3 str "$V" changed 600 timeout wait >/dev/null || break
  done
  fux %3 str last_status get; fux %3 last-output capture
  ```
- **A shell command's end, without marks:** print a text only its end can
  show, quoted so the line you type does not hold it, and wait for it as
  above: `str "make test; echo END-OF''-RUN \$?" line` (`$status` in fish)
  shows `END-OF''-RUN` as you type it and `END-OF-RUN 0` at its end.
- **A program's exit:**

  ```sh
  V=$(fux opts str 'cargo test' command run | cut -d' ' -f2); P=%2
  until fux $P str "$V" changed 600 timeout wait | grep -qE 'exited|killed'; do
    V=$(fux $P str version get)
  done
  fux $P str exited get
  ```
- **A bell or notification:** the version's third count grows.
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux %2 all-history capture` reads
  all of it.
- **Whether a program waits for keys:** `str reading get` says whether a
  thread of its foreground is blocked reading its terminal, where the
  system says (Linux; `null` on macOS). A program waiting in poll or
  select, as most full-screen ones do, is not reading.
- **A pane's history in your editor:** `f=$(mktemp) && fux %N all-history
  capture > "$f" && $EDITOR "$f"`. On a key, in `config.fux` (`env` gives
  the program the pane's id):

  ```sh
  define edit-history
    me focused vertical
    str FUX_TARGET me focused env
    str sh program str -c arg
    str 'f=$(mktemp) && fux "$FUX_TARGET" all-history capture >"$f" && ${EDITOR:-vi} "$f"; rm -f "$f"; fux here kill' arg
    split drop
  end
  str y str edit-history bind  str 'edit history' label  str Panes group
  ```

## keys: Keys, the mouse and stopping

- **Keys** go between the pane and `send-keys`: `str T text`, `str NAME
  key` (tmux's names: `Enter`, `Escape`, `Tab`, `BSpace`, `Up`, `PageDown`,
  `F5`, `C-c`, `M-s`, `S-Tab`; `Esc`, `Backspace`, `PgUp` too; a typo is
  refused, naming the closest), `str T paste` (bracketed if the program
  asked), `str FILE paste-file`, `str HEX bytes` (`1b5b41`), `N times`
  (repeats the input just before it). 900 KiB at most in all.
- They are written together, in order, as the program's terminal takes
  them. A program that tells a lone Escape from a key's sequence by time
  may read `Escape` and the key after it as one key: send them in two
  `send-keys`. Keys sent to a program that exited are refused
  (`not_running`).
- **Mouse:** `%2 str click ROW COL mouse send-mouse` (also `right`,
  `middle`, `press`, `release`, `wheel-up`, `wheel-down`, `move`), cells
  from 0 as `wrapped capture` counts them; on a double-width row, click at
  2×COL. A program's own "row 22" is usually counted from 1.
- **Stop:** `%2 str TERM signal` (`INT`, `KILL`, …) signals what runs in
  its terminal's foreground; `%2 kill` closes the pane; `%2 respawn` starts
  its program again (`kill-first` ends it first).

## read: Reading the screen

`PANE READ capture`, READ any of: `100 history`, `all-history`,
`main-screen` (what was printed before a program took the screen),
`alt-screen` (the full screen a program drew last), `last-output` (the
last shell command's output, or the running one's so far, between its
marks), `wrapped` (each screen row a line; else a line wrapped at the
pane's width is joined whole), `0 4 rows`, `0 39 cols`, `full`, `marks`
(reverse video and any background colour as `⟦this⟧`: a program that
paints its whole background is marked whole), `styles`; then `json`
for the screen and what is known of the pane: `cursor` is `[ROW,COL]`,
`alt_screen` whether a program holds the alternate screen, `history_lines`
the history kept, `above` the rows kept above what was read, `idle_ms`
since its program last wrote. Rows and columns count from 0.

## shells: Shells and their marks

`fux opts str agent in new-tab` gives a shell. fux starts shells as they
are; a shell that sends OSC 133 marks (`C` as a command's output begins,
`D;STATUS` as it ends, `A` at a prompt) counts the commands that end in
its pane's version (`recipes`), gives `last_status`, lets `last-output`
read a command's output alone and copy mode's `[` and `]` jump between
prompts. fux reads marks from any program that sends them. Without marks
the count stays, and `last-output` says there are none. To send them (in your own startup
files):

```sh
# bash 4.4 or later (macOS's /bin/bash 3.2 has no PS0), ~/.bashrc
PS0='\e]133;C\a'
PROMPT_COMMAND='printf "\e]133;D;%s\a\e]133;A\a" "$?"'${PROMPT_COMMAND:+;$PROMPT_COMMAND}
# zsh, ~/.zshrc
autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
add-zsh-hook precmd mark_end
# fish, ~/.config/fish/config.fish
function mark_start --on-event fish_preexec; printf '\e]133;C\a'; end
function mark_end --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
function mark_prompt --on-event fish_prompt; printf '\e]133;A\a'; end
```

A remote shell (`ssh`) marks commands if its startup does. Or run a remote
command in a pane of its own, and wait for its exit: `fux opts str 'ssh -n
HOST make' command run`.

## programs: A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```sh
# test.fux
opts str tests name str bash program run let T
T str 'cargo test' line drop
T all-history capture
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
0, or with the status of the failure that stopped it.

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
  `capture-client` shows it): how to drive fux's own screens.
