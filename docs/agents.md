# Driving programs with fux

(`fux help-agents`: this screen, enough for most work. A topic at length:
`fux help agents TOPIC`, below; `fux help` lists every word, `fux help WORD`
explains one.)

A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed. The server is
`FUX_SOCKET`'s; `fux start` starts one if none runs. **The word that acts
comes last:** what it takes (keys, a version, what to read) goes on the
pane before it, and `json` after it. Only `run`, `new-tab` and
`new-workspace` start from `opts`; of the three, only `run` takes a
program (as `split` does): the other two start a shell (`str 'CMD' typed`
types a line into it).

**Every pane has a version, and a wait waits for it to change.** A version
(`0.57.1.3.0`, `STARTS.SCREEN.BELLS.DONE.ENDED`) counts what only grows
while one program runs (`respawn` starts all but the first afresh, a
server restart all of them): its program's starts (`respawn`), its
screen's revision (one more for each piece of output read, each resize to
another size, each change to its history: cleared, or `history-bytes`
set), bells and notifications rung, shell commands ended (OSC 133 marks),
and 1 once its program ended. Every action
(`run`, `send-keys`, `send-mouse`, `signal`, `split`, `new-tab`,
`new-workspace`, `respawn`) acts at once and prints its pane and the
version from just before it acted (a new pane's, or a respawned one's, as
it starts): wait from that one, and nothing its input brings is missed.
fux never judges whether a program is done: which change means done is
yours to say (recipes below).

```sh
fux opts str web name 100 30 size str ./app program str --fast arg run  # start: %2 0.1.0.0.0
fux %2 str y text str Enter key send-keys      # keys: %2 0.57.0.3.0, its version before them
fux %2 str 'a long prompt' paste send-keys     # a paste, bracketed if the program asked
fux %2 str 0.57.0.3.0 changed wait             # until it differs: %2 0.61.0.3.0 screen
fux %2 wait                                    # until it differs from now's
fux %2 str y text send-keys wait               # in one program: from the answer's version
fux %2 str 0.61.0.3.0 changed 600 timeout wait # 600s at most (else 30, the server's wait-seconds)
fux %2 all-history capture                     # read the screen; also main-screen, last-output, styles
fux %2 capture json                            # as JSON, with version, exited, idle_ms
fux %2 str version get                         # one field: version, exited, last_status, cwd
fux %2 str C-c key send-keys                   # a key: Enter, C-c, Up, F5 (`fux key-names`)
fux %2 str click 3 10 mouse send-mouse         # a click at row 3, column 10, from 0
fux %2 tab 80 24 resize; fux %2 50 percent resize-x; fux %2 horizontal split
fux %2 str TERM signal                         # signal what runs in its foreground
fux %2 kill-first respawn                      # end its program and start it again
fux str web pane                               # a pane by its tab's name
fux ls                                         # what there is: workspaces, tabs, panes
fux str agent workspace kill                   # at the end: everything run started
```

A wait answers the pane, its version and what changed: `%2 0.61.1.4.0
screen bell done`, with `exited 3` or `killed by SIGTERM` if its program
ended since, `respawned` once started again (its counts then start
afresh). A wait from a version that already holds the end waits for
something else, as any wait does. On a tab or workspace it waits on the
panes there as it begins, from each one's version then, answering those
that changed. Status: 0 changed; 1 failed (`fux: CODE: MESSAGE` on
stderr); 4 the pane closed (a shell pane closes when its shell exits); 124
nothing changed in time (`%2 0.61.1.4.0 timeout`). Clean up with `fux str
agent workspace kill`; stop what you start by its pid, never `pkill -f`.

Topics (`fux help agents TOPIC`; `all` for the whole guide): `recipes`,
`keys`, `read`, `shells`, `programs`, `servers`.

## recipes: Waiting for what you need

Each is a line or two of bash around three helpers. `step` waits from the
version in `V` for pane `P` to change, at most `$1` seconds, and puts the
new version in `V`; `V` is always from before what you wait for, so a
change between two calls is never missed. `wait_for SECS CHECK…` steps
until the command CHECK succeeds, checking before each step and once more
after the last, so what came with the program's end counts. `quiet SECS
N` steps until no change comes for N seconds. Each answers 0 once what
it waited for came, else why not: 124 time is up, 2 its program ended, 4
the pane closed, 1 fux failed (stderr says why).

```sh
step() {
  local out; out=$(fux "$P" str "$V" changed "$1" timeout wait) || return
  V=${out#* }; V=${V%% *}
  case ${V##*.} in 1) return 2 ;; esac
}
wait_for() {
  local end=$((SECONDS + $1)) s=0; shift
  until "$@"; do
    [ "$s" = 0 ] || return "$s"
    [ "$SECONDS" -lt "$end" ] || return 124
    s=0; step $((end - SECONDS)) || s=$?
  done
}
quiet() {
  local end=$((SECONDS + $1)) s
  while [ $((end - SECONDS)) -ge "$2" ]; do
    s=0; step "$2" || s=$?
    case $s in 0) ;; 124) return 0 ;; *) return "$s" ;; esac
  done
  return 124
}
```

- **A text the reply writes:** count it before, act, and wait until there
  is one more. Choose a text only the reply holds: here the question holds
  no `42`. If the input itself holds it, wait until the capture shows the
  input, then count, so it is in the count, not in what you wait for; a
  program that draws the input again (a TUI that keeps it above the
  reply) counts it again. `grep -c` counts lines, and history beyond
  `history-bytes` is dropped, so a count over a long run can fall.

  ```sh
  read -r P V < <(fux %2 str 'What is 6 times 7? Digits only.' paste send-keys)
  n=$(fux "$P" all-history capture | grep -c 42 || :)  # grep -c fails on 0
  more() { [ "$(fux "$P" all-history capture | grep -c 42)" -gt "$n" ]; }
  read -r P V < <(fux "$P" str Enter key send-keys)
  wait_for 600 more && echo answered
  ```
- **Quiet for N seconds** (an agent's TUI done, or asking you something;
  a program that ends meanwhile answers 2, not quiet):
  a spinner or clock on the screen is a change, so then wait for a text.

  ```sh
  read -r P V < <(fux %2 str 'fix the test' paste str Enter key send-keys)
  quiet 600 5 && fux "$P" all-history capture
  ```
- **A shell command's end, with marks** (`shells` says how to turn them
  on): the version's fourth count, `DONE`, grows as one ends. Type the
  line at a prompt: a command still running ends first, and counts.

  ```sh
  read -r P V < <(fux %3 str 'make test' line); D=$(cut -d. -f4 <<<"$V")
  ended() { [ "$(cut -d. -f4 <<<"$V")" -gt "$D" ]; }
  wait_for 600 ended && fux "$P" str last_status get && fux "$P" last-output capture
  ```
- **A shell command's end, without marks:** print a text only its end can
  show, quoted so the line you type does not hold it, and wait for it as
  for a text: `str "make test; echo END-OF''-RUN \$?" line` (`$status` in
  fish) shows `END-OF''-RUN` as you type it and `END-OF-RUN 0` at its end.
- **A program's exit:** the version's last count, `ENDED`, is 1.

  ```sh
  read -r P V < <(fux opts str 'cargo test' command run)
  exited() { [ "${V##*.}" = 1 ]; }
  wait_for 600 exited && fux "$P" str exited get
  ```
- **A bell or notification:** the version's third count, `BELLS`, grows:
  as for a command's end, with `-f3`.
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux %2 all-history capture` reads
  all of it.
- **Whether a program waits for keys:** fux does not say; no system
  reports it as a fact. Wait for what the program shows when it asks (a
  prompt's text), or for quiet.
- **A pane's history in your editor:** `f=$(mktemp) && fux %N all-history
  capture > "$f" && $EDITOR "$f"`. On a key, in `config.fux` (`env` gives
  the program the pane's id; `"${FUX:-fux}"` is the fux the server runs,
  on PATH or not):

  ```sh
  define edit-history
    me focused vertical
    str FUX_TARGET me focused env
    str sh program str -c arg
    str 'f=$(mktemp) && "${FUX:-fux}" "$FUX_TARGET" all-history capture >"$f" && ${EDITOR:-vi} "$f" || read -r _; rm -f "$f"; "${FUX:-fux}" here kill' arg
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
  `send-keys`. Keys sent to a program that exited, that closed its
  terminal (where the system says so: Linux), or that is paused since a
  restart are refused (`not_running`).
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
marks: what a clear erased there reads blank, and an ended command's
last row ends at its end mark, so a last line ending in `\r` reads
empty), `wrapped` (each screen row a line; else a line wrapped at the
pane's width is joined whole), `0 4 rows`, `0 39 cols`, `full`, `marks`
(reverse video and any background colour as `⟦this⟧`: a program that
paints its whole background is marked whole), `styles`; then `json`
for the screen and what is known of the pane: `cursor` is `[ROW,COL]`,
`alt_screen` whether a program holds the alternate screen, `history_lines`
the history kept, `above` the history rows kept above the first row read,
`idle_ms` since its program last wrote. Rows and columns count from 0.

## shells: Shells and their marks

`fux opts str agent in new-tab` gives a shell. fux starts shells as they
are; a shell that sends OSC 133 marks (`C` as a command's output begins,
`D;STATUS` as it ends, `A` at a prompt) counts the commands that end in
its pane's version (`recipes`), gives `last_status`, lets `last-output`
read a command's output alone and copy mode's `[` and `]` jump between
prompts. fux reads marks from any program that sends them. Without marks
the count stays, and `last-output` says there are none. To send them (in
your own startup files):

```sh
# bash 4.4 or later (macOS's /bin/bash 3.2 has no PS0), ~/.bashrc
PS0='\e]133;C\a'
PROMPT_COMMAND='printf "\e]133;D;%s\a\e]133;A\a" "$?"'${PROMPT_COMMAND:+;$PROMPT_COMMAND}
# zsh, ~/.zshrc
autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
precmd_functions=(mark_end $precmd_functions)  # first, so $? is the command's
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
# test.fux: run the tests, wait until they end, read what they printed
opts str tests name str 'cargo test' command run   # its answer: %2 0.1.0.0.0
begin
  dup string str ' ' split-on 1 at str . split-on last str 1 eq   # ended: last count 1
  if true else 600 timeout wait false then     # a wait on an answer waits from its version
until
all-history capture
```

`X let NAME` names a value, `define NAME … end` makes a word, `if … else …
then` and `begin … until` decide and repeat, `list … end` gathers what its
body leaves, `SEQ each … end` runs once per element. An action leaves its
answer: printed, it is the lines above; given to a word that wants a pane,
it is its pane (an action's or wait's on a tab or workspace is none,
however many panes it has: name the tab again, `@1 wait`), and to `wait`,
the version it holds too (`T str y text
send-keys wait` waits from before the keys; a name keeps that version, so
`T wait` later waits from when `T` was made, and `T dup str version get
changed wait` from now); `string` makes it text (`%2 screen string str error
contains`). The standard library, written in fuxth, does the rest (`fux
help` lists it). A mistake every run would make (too few values, a string
given to `add` or `if`, a request nothing acts on: its error names the word
to end it with) fails before any word acts; otherwise the words before a
failing one have already acted. A run exits 0, or with the status of the
failure that stopped it.

## servers: Servers, sizes and more

- `str NAME server` (`NAME.sock` in fux's directory) or `str PATH socket`
  first chooses another server; `start` starts one, `status` says whether
  one answers. Inside a person's pane, `here` is that pane. Every pane
  has `FUX_PANE` (its id), `FUX_SOCKET` (its server's socket, or a grant's
  if started through one), `FUX_SERVER` (which run of the server: from a
  pane of another, a command has no `here`, `stray_pane`) and `FUX` (the
  fux the server runs, for a script that may not find `fux` on PATH; unset
  if the server cannot name it). A person can give you `str agent str PATH
  grant`: a socket that reaches workspace `agent` alone. `kill-server` from
  inside its own server is refused; `force-kill-server` stops it.
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
