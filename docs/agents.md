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

**Actions answer at once; waiting is a word you say.** `run`, `send-keys`,
`signal` and the rest act and answer with their pane. fux reports what
happened and never judges whether a program is done: you say what to wait
for. Wait words on the pane before an action wait from the moment it acts;
`wait` waits from when it runs.

```sh
fux opts str web name 100 30 size str ./app program str --fast arg run  # start: answers %2 at once
fux opts str ./app program str 'listening on' wait-text run  # start, then wait for a text it writes
fux opts str 'cargo test' command wait-exit run   # start, then wait for its end
fux %2 str y text str Enter key send-keys      # keys (str T paste send-keys pastes): answers at once
fux %2 str 'Saved' wait-text str C-s key send-keys   # keys, then wait for a text the reply holds
fux %3 600 timeout wait-done str 'make test' line   # a shell command, until it ends (shell marks)
fux %2 5 wait-quiet wait                       # until it writes nothing for 5s
fux %2 all-history capture                     # read the screen; also main-screen, last-output, styles
fux %2 capture json                            # as JSON; `%2 str exited get` is one field
fux %2 str C-c key send-keys                   # a key: Enter, C-c, Up, F5 (`fux key-names`)
fux %2 str click 3 10 mouse send-mouse         # a click at row 3, column 10, from 0
fux %2 tab 80 24 resize; fux %2 50 percent resize-x; fux %2 horizontal split
fux %2 str TERM signal; fux %2 respawn         # stop it; start it again
fux str web pane                               # a pane by its tab's name
fux ls                                         # what there is: workspaces, tabs, panes
fux str agent workspace kill                   # at the end: everything run started
```

What to wait for, any of them, whichever comes first (each after the wait
began), and how long at most:

```
str T wait-text   it writes T: exactly, escape sequences aside, your keys' echo too
wait-change       it writes anything
N wait-quiet      it writes nothing for N seconds
wait-exit         it exits (or has)
wait-bell         it rings the bell or sends a notification
wait-done         a shell command ends, as its shell's OSC 133 marks say
N timeout         how long at most; else 30s (the server's wait-seconds)
```

A wait answers `%2 text "T"` and the line it was written on, `%2 changed`,
`%2 quiet`, `%2 exited 3`, `%2 bell`, `%2 notification "…"` or `%2 done
0`. Status: 0 met; 1 failed (`fux: CODE: MESSAGE` on stderr); 3 the
program exited, or was started again, first; 4 the pane closed; 124 the
time limit came first. Clean up with `fux str agent workspace kill`; stop
what you start by its pid, never `pkill -f`.

Topics (`fux help agents TOPIC`; `all` for the whole guide): `recipes`,
`waits`, `keys`, `read`, `shells`, `programs`, `servers`.

## recipes: Waiting for what you need

fux gives the facts; which one means "done" is yours to choose. Some ways:

- **A shell command, with marks** (`shells` says how to turn them on):
  `fux %3 600 timeout wait-done str 'make test' line` answers `%3 done 0`
  as it ends; `fux %3 last-output capture` reads its output alone.
- **A shell command, without marks:** print a text only its end can show,
  quoted so the line you type does not hold it: `fux %3 str END-OF-RUN
  wait-text str "make test; echo END-OF''-RUN \$?" line`. The echo of
  your line shows `END-OF''-RUN`; the output, `END-OF-RUN 0`.
- **A full-screen agent's turn** (Claude Code, Codex): its spinner redraws
  while it works, so wait until it writes nothing for a few seconds:
  `fux %2 600 timeout 5 wait-quiet str 'fix the test' paste str Enter key
  send-keys`. A question it asks you is quiet too: read the screen.
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux %2 all-history capture` reads
  all of it.
- **A prompt a program draws** (`>>> `, `(gdb) `): wait for the prompt's
  text after sending a line; the line you typed does not hold it.
- **A server that starts:** `fux opts str ./serve program str 'listening
  on' wait-text run`.
- **A text your keys also show:** your keys' echo is written too, and a
  full-screen program draws what you type as its own output (an agent's
  TUI shows your prompt). Wait for a text only the reply can hold.
- **A pane's history in your editor:** `f=$(mktemp) && fux %N all-history
  capture > "$f" && $EDITOR "$f"`. On a key, in `config.fux` (`env` gives
  the program the pane's id):

  ```sh
  define edit-history
    me focused vertical
    str FUX_TARGET me focused env
    str sh program str -c arg
    str 'f=$(mktemp) && fux "$FUX_TARGET" all-history capture >"$f" && ${EDITOR:-vi} "$f"; rm -f "$f"' arg
    split drop
  end
  str y str edit-history bind  str 'edit history' label  str Panes group
  ```

## waits: Time, and waiting for a text

- A wait waits 30s at most (the server's `wait-seconds`); `SECS timeout`
  on the pane changes it for one. A `wait` on a tab or workspace waits in
  each pane, a line each, the first failing status naming the whole.
- Wait words on the pane before an action (`send-keys`, `send-mouse`,
  `signal`, `run`, `split`, `respawn`) wait from the moment it acts, so
  nothing its input brings is missed. `wait` alone waits from when it runs:
  what came before is not looked at.
- `wait-text` reads the bytes the program wrote, escape sequences and
  control characters left out, a carriage return or a line feed ending a
  line, and finds the text exactly as written: case, blanks and all. Drawn
  in pieces (a word in two colours) it is still found; drawn with cursor
  moves between its letters, it is not. Met, the answer's second line is
  the line it was written on, as written so far.
- `wait-quiet` counts from the wait's start or the last byte written,
  whichever is later: a clock or a spinner is never quiet, and its wait
  times out (124).
- A wait that fails (3, 4, 124) says why on stderr; its first line still
  names the pane and what happened (`%2 exited 1`, `%2 timeout`). A met
  `wait-exit` is status 0; the program's own status is in the answer
  (`exited 3`) and in `str exited get`.

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
(reverse video and background colours as `⟦this⟧`), `styles`; then `json`
for the screen and what is known of the pane: `cursor` is `[ROW,COL]`,
`alt_screen` whether a program holds the alternate screen, `history_lines`
the history kept, `above` the rows kept above what was read, `idle_ms`
since its program last wrote. Rows and columns count from 0.

## shells: Shells and their marks

`fux opts str agent in new-tab` gives a shell. fux starts shells as they
are; a shell that sends OSC 133 marks (`C` as a command's output begins,
`D;STATUS` as it ends, `A` at a prompt) lets `wait-done` wait for a
command, `last-output` read its output alone and copy mode's `[` and `]`
jump between prompts. fux reads marks from any program that sends them.
Without marks, `wait-done` waits to its limit and says no marks came,
and `last-output` says there are none. To send them (in your own startup
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
command in a pane of its own: `fux opts str 'ssh -n HOST make' command
wait-exit run`.

## programs: A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```sh
# test.fux
opts str tests name str bash program run let T
T 600 timeout wait-done str 'cargo test' line drop
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
  `capture-client` shows it): how to drive fux's own menus.
