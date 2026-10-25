# Driving programs with fux

(`fux help-agents`: this screen, enough for most work. A topic at length:
`fux help agents TOPIC`, below; `fux help` lists every word, `fux help WORD`
explains one.)

A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed. The server is
`FUX_SOCKET`'s; `fux start` starts one if none runs. **The word that acts
comes last:** what it takes (keys, a wait's time, what to read) goes on the
pane before it, and `json` after it. Only `run`, `new-tab` and
`new-workspace` start from `opts`; of the three, only `run` takes a program
(as `split` does): the other two start a shell.

**Every pane has a version, and a wait waits for it to change.** A version
is one word, `%2:0.57.1.3.0:48211-1867a3f9c2b8e7d6`: its pane, its counts
`RESPAWNS.SCREEN.BELLS.DONE.ENDED`, and the run of the server that gave it
(a version from another server, or from before a restart, is refused). The
counts only grow while one program runs (`respawn` starts all but the first
afresh): how often its program was respawned, its screen's revision (the
bytes of output read, plus one for each resize to another size and each
change to its history: cleared, or `history-bytes` set; the same however
the output was cut into reads), bells and notifications rung, shell
commands ended (OSC 133 marks), and 1 once its program ended. Every action
(`run`, `send-keys`, `paste-buffer`, `send-mouse`, `signal`, `split`,
`new-tab`, `new-workspace`, `respawn`) acts at once and prints the version
of its pane from just before it acted (a new pane's, or a respawned one's,
as it starts): wait from that one, and nothing its input brings is missed.
fux never judges whether a program is done: which change means done is
yours to say (recipes below).

```sh
A=$(fux opts str web name 100 30 size str ./app program str --fast arg run)  # %2:0.0.0.0.0:RUN
fux str "$A" changed wait                      # until it differs: %2:0.61.0.0.0:RUN screen
A=$(fux %2 str y text str Enter key send-keys) # keys: the version from before them
fux str "$A" changed 600 timeout wait          # 600 s at most (else 30, the server's wait-seconds)
fux %2 str y text send-keys wait               # in one program: from the answer's version
fux %2 str y text send-keys 5 quiet            # until nothing changes there for 5 s
fux %2 wait                                    # until it differs from now's
fux %2 str 'a long prompt' paste send-keys     # a paste, bracketed if the program asked
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

A wait answers its pane's version now and what changed:
`%2:0.61.1.4.0:RUN screen bell done`, with `exited 3` or `killed by
SIGTERM` if its program ended since, `respawned` once started again (its
counts then start afresh). Its first word is the version to wait from next.
A wait from a version that already holds the end waits for something else,
as any wait does; one from a version of a pane since closed answers
`%2 closed`. On a tab or workspace a wait waits on the panes there as it
begins, from each one's version then, answering those that changed.
Status: 0 changed; 1 failed (`fux: CODE: MESSAGE` on stderr); 4 the pane
closed (a shell pane closes when its shell exits); 124 nothing changed in
time (`%2:0.61.1.4.0:RUN timeout`). In a program, `str timeout try … catch
… end` goes on after a failure instead (`programs`). Clean up with `fux str
agent workspace kill`; stop what you start by its pid, never `pkill -f`.

Topics (`fux help agents TOPIC`; `all` for the whole guide): `recipes`,
`keys`, `read`, `shells`, `programs`, `servers`.

## recipes: Waiting for what you need

Each is a fuxth program: a file `fux FILE` runs, or `fux - <<'EOF'` in a
script. `A wait` waits from the version answer A holds, so a change between
two steps is never missed, and leaves the next answer; `A 600 timeout wait`
waits 600 s at most, else fails with 124 (`timeout`). A loop of waits ends
when its check holds; to bound the whole of it, run it under the shell's
`timeout 600 fux FILE` (124 too). `A counts` gives a version's five counts
as integers: `A counts 3 at` is its `DONE`.

- **Quiet for N seconds** (an agent's TUI done, or asking you something):
  `quiet` waits until nothing changes for N seconds (from a pane, an
  answer, or `str "$A" changed`). A spinner or clock on the screen is a
  change, so then wait for a text.

  ```fux
  %2 str 'fix the test' paste str Enter key send-keys 5 quiet
  %2 all-history capture
  ```
- **A text the reply writes:** count it before, act, and wait until there
  is one more. Choose a text only the reply holds: here the question holds
  no `42`. If the input itself holds it, wait until the capture shows the
  input, then count, so it is in the count, not in what you wait for; a
  program that draws the input again (a TUI that keeps it above the
  reply) counts it again. `hits` counts the lines of the last 1000 of
  history and the screen (a string holds at most 1,000,000 characters,
  less than `history-bytes` may keep), so a count can fall as lines
  scroll past them: choose a window the reply surely comes within.

  ```fux
  define hits 1000 history capture string lines 0 swap each str 42 contains if 1 add then end end
  %2 hits let n
  %2 str 'What is 6 times 7? Digits only.' paste str Enter key send-keys
  begin 600 timeout wait %2 hits n gt until
  drop str answered
  ```
- **A shell command's end, with marks** (`shells` says how to turn them
  on): its version's `DONE` grows as one ends. Type the line at a prompt:
  a command still running ends first, and counts. A line that runs nothing
  (an empty line, a comment, a syntax error) ends no command, so `DONE`
  stays: for such a line, wait for the prompt's text instead. (A comment
  line ends a command in fish, which marks it, and in zsh, which runs `#`
  as a command unless `setopt interactive_comments`; a syntax error is
  tested in bash and zsh alone.)

  ```fux
  %3 str 'make test' line let A
  A counts 3 at let done
  begin A 600 timeout wait let A  A counts 3 at done gt until
  %3 str last_status get print
  %3 last-output capture
  ```
- **A shell command's end, without marks:** print a text only its end can
  show, quoted so the line you type does not hold it, and wait for it:
  the line shows `END-OF""-RUN` as you type it, and `END-OF-RUN 0` at its
  end (`$status` in fish).

  ```fux
  %3 str 'make test; echo END-OF""-RUN $?' line
  begin 600 timeout wait %3 screen string str END-OF-RUN contains until
  drop %3 screen
  ```
- **A program's exit:** its version's `ENDED` is 1. The loop waits before
  it looks, so it wants an answer from before the end (`run`'s).

  ```fux
  opts str 'cargo test' command run
  begin 600 timeout wait dup counts 4 at 1 eq until
  str exited get
  ```
- **A bell or notification:** its version's `BELLS`, `counts 2 at`, grows,
  as for a command's end.
- **A deadline, a fallback, a retry:** `try` catches the failures it names
  (`programs`). A key, and C-c if nothing came of it in 10 s:

  ```fux
  %2 str y text send-keys let A
  str timeout try A 10 timeout wait drop catch 2drop %2 str C-c key send-keys drop end
  ```
  A command's output alone where the shell marks commands, else the screen:
  `str no_marks try %3 last-output capture catch 2drop %3 capture end`.
- **A shell to type into:** `shell` makes a tab with one in workspace
  `agent`. Wait for its prompt (a text it shows) before you type: its
  startup may drop what comes sooner. A command that must run is surer
  started as the program: `opts str sh program str -c arg str 'make test;
  exec "${SHELL:-sh}"' arg run` (fux gives no `SHELL` of its own).
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux %2 all-history capture` reads
  all of it.
- **Whether a program waits for keys:** fux does not say; no system
  reports it as a fact. Wait for what the program shows when it asks (a
  prompt's text), or for quiet.

## keys: Keys, the mouse and stopping

- **Keys** go between the pane and `send-keys`: `str T text`, `str NAME
  key` (tmux's names: `Enter`, `Escape`, `Tab`, `BSpace`, `Up`, `PageDown`,
  `F5`, `C-c`, `M-s`, `S-Tab`; `Esc`, `Backspace`, `PgUp` too; a typo is
  refused, naming the closest), `str T paste` (bracketed if the program
  asked), `str FILE paste-file`, `str HEX bytes` (`1b5b41`), `N times`
  (repeats the input just before it). 900 KiB at most in all.
- They are written together, in order, as the program's terminal takes
  them; an action answers once they are queued, not once the program
  read them. A program that tells a lone Escape from a key's sequence by
  time (Vim, readline) may read Escape and a key sent soon after it, in one
  `send-keys` or two, as one key: send Escape, wait until the program shows
  it took it (its mode line), then the next key. Keys sent to a program
  that exited, that closed its terminal (where the system says so: Linux),
  or that is paused since a restart are refused (`not_running`).

  ```fux
  %2 str Escape key send-keys
  begin 5 timeout wait %2 capture string str INSERT contains not until drop
  %2 str :wq text str Enter key send-keys
  ```
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
paints its whole background is marked whole), `styles`. Blanks at a
line's end are trimmed, so a prompt's `>>> ` reads `>>>`; `full` keeps
every cell. Then `json` for the screen and what is known of the pane:
`cursor` is `[ROW,COL]`, `alt_screen` whether a program holds the
alternate screen, `history_lines` the history kept, `above` the history
rows kept above the first row read, `idle_ms` since fux last read output
from its program (or it started). Rows and columns count from 0.

## shells: Shells and their marks

`fux opts str agent in new-tab` gives a shell. fux starts shells as they
are; a shell that sends OSC 133 marks (`C` as a command's output begins,
`D;STATUS` as it ends, `A` at a prompt) counts the commands that end in
its pane's version (`recipes`), gives `last_status`, lets `last-output`
read a command's output alone and copy mode's `[` and `]` jump between
prompts. fux reads marks from any program that sends them. Without marks
the count stays, and `last-output` says there are none. A `D` with no `C`
before it ends nothing: a line that runs nothing (an empty line; a
syntax error in bash and zsh; a comment in bash) runs no command, so it
sends no `C`, and `DONE` and `last_status` stay. fish marks a comment line
as a command, and zsh runs `#` as one unless `setopt
interactive_comments`. To send them, in your own startup files:

```bash
# bash 4.4 or later (macOS's /bin/bash 3.2 has no PS0), ~/.bashrc
PS0='\e]133;C\a'
mark_end() { local s=$?; printf '\e]133;D;%s\a\e]133;A\a' "$s"; return "$s"; }
PROMPT_COMMAND=mark_end${PROMPT_COMMAND:+;$PROMPT_COMMAND}  # first: the hooks after it see $?
```

```zsh
# zsh, ~/.zshrc
autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
precmd_functions=(mark_end $precmd_functions)  # first, so $? is the command's
```

```fish
# fish, ~/.config/fish/config.fish
function mark_start --on-event fish_preexec; printf '\e]133;C\a'; end
function mark_end --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
function mark_prompt --on-event fish_prompt; printf '\e]133;A\a'; end
```

In bash, a hook after `mark_end` sees the command's `$?`, though
`PIPESTATUS` then holds `$?` alone. A remote shell (`ssh`) marks commands if
its startup does. Or run a remote command in a pane of its own, and wait
for its exit: `fux opts str 'ssh -n HOST make' command run`.

## programs: A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```fux
# test.fux: run the tests, wait until they end, read what they printed
opts str tests name str 'cargo test' command run   # its answer: %2:0.0.0.0.0:RUN
begin 600 timeout wait dup counts 4 at 1 eq until  # ENDED, its last count, is 1
all-history capture
```

`X let NAME` names a value, `define NAME … end` makes a word, `if … else …
then` and `begin … until` decide and repeat, `list … end` gathers what its
body leaves, `SEQ each … end` runs once per element, `CODES try … catch …
end` runs its handler instead of failing with one of CODES (`str timeout
try %2 5 timeout wait catch … end`: its body starts with an empty stack;
the handler, with the stack as at `try` and the failure's message and code
on it); a failure it does not name goes on, its status kept. An action
leaves its answer: printed, it is the lines above; given to a word that
wants a pane, it is its pane (an action's or wait's on a tab or workspace
is none, however many panes it has: name the tab again, `@1 wait`), and to
`wait`, the version it holds too (`T str y text send-keys wait` waits from
before the keys; a name keeps that version, so `T wait` later waits from
when `T` was made, and `T str version get changed wait` from now);
`string` makes it text (`%2 screen string str error contains`). The
standard library, written in fuxth, does the rest (`fux help` lists it). A
mistake every run would make (too few values, a string given to `add` or
`if`, a request nothing acts on: its error names the word to end it with)
fails before any word acts; otherwise the words before a failing one have
already acted. A run exits 0, or with the status of the failure that
stopped it.

## servers: Servers, sizes and more

- `str NAME server` (`NAME.sock` in fux's directory) or `str PATH socket`
  first chooses another server; `start` starts one, `status` says which
  answers, and fails (`no_server`) where none does: `fux status || fux
  start`. Inside a person's pane, `here` is that pane. Every pane has
  `FUX_PANE` (its id), `FUX_SOCKET` (its server's socket, or a grant's if
  started through one), `FUX_SERVER` (which run of the server: a
  `FUX_PANE` of another, or with no `FUX_SERVER`, is no `here`:
  `stray_pane`) and `FUX` (the fux the server runs, for a script that may
  not find `fux` on PATH; unset if the server cannot name it). A person
  can give you `str agent str PATH grant`: a socket that reaches workspace
  `agent` alone. `kill-server` from inside its own server is refused;
  `force-kill-server` stops it.
- A server that did not stop cleanly leaves its layout in
  `$XDG_STATE_HOME/fux`: the next server on that socket brings it back, each
  program waiting for `respawn`. `kill-server` forgets it. It is another
  server run: versions from before it are refused.
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
