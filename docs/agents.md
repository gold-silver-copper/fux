# Driving programs with fux

Use your assigned private `FUX_SOCKET`. Beside a human, use a private
socket, `XDG_RUNTIME_DIR` (mode 0700) and `XDG_CONFIG_HOME`; a workspace
alone is not isolation. `fux start` starts an empty server.

Words run left to right. `str WORD` pushes text; %2 is a pane, @2 a tab,
+1 a workspace. Options precede the action. Remaining values print, a
value a line (a list an item a line, a list in it as JSON).
Outside fux, give new tabs a workspace and a size: `opts str agent in
80 24 size`.

**Launch and wait for a prompt.** Run with `timeout 30 fux -` and this
program on stdin; a banner alone is not the prompt:

```fux
opts str agent in 80 24 size str python3 program str -q arg new-tab
str '>>>' 5 shows let seen
seen string print seen version print
```

**Run a command, read its output and actual exit status.**
Run with `timeout 30 fux -` and this program on stdin:

```fux
opts str agent in 80 24 size str sh program str -c arg
str 'echo result=42; exit 7' arg new-tab
10 until-ended let seen
seen string print seen str exited get print
```

**Guard input against the screen you read.** Save this as `reply.fux` and
run `timeout 30 fux --file reply.fux "$V"`, using the prompt version above:

```fux
args first expect str '6*7' text str Enter key send-keys
str 42 5 shows let seen
seen string print seen version print
```

**Cleanup:** `fux str agent workspace kill` closes only that workspace on
your private socket. Stop any subscriber or server you start by its PID.

`shows` checks text presence; old text and input echo can match. Use a
reply-only result (42 is absent from 6*7), shell end marks, or a TUI-specific
predicate. `quiet` returns its last capture after an explicit unchanged
interval, which does not prove computation finished. `until-ended` checks
ENDED before waiting, including an already-ended result, and follows the
pane if respawned. Each wait timeout is **per change**; use shell `timeout`
for a whole loop. A capture keeps screen, version and metadata together;
`get` and `version` on it read locally. `observed` retains a pane capture
or samples a pane/single answer; these helpers reject client captures.

An action answers its version BEFORE acting; `changed ... wait` reports
changes, never readiness. `counts` reads RESPAWNS SCREEN BELLS DONE ENDED.
`grows` accepts a pane (sampled first), version text, answer or pane capture,
count index 1..4, nonnegative growth and seconds; it fails across respawn.
Exit status and signal are distinct; unknown status stays unknown.
A failed run prints nothing more on stdout: stderr says `fux: CODE:
MESSAGE`, then what was left. A transport failure may leave input queued.

| fux exits | when |
| --- | --- |
| 0 | done, or what `N exit` gave |
| 1 | refused: any other failure |
| 2 | a mistake in the program: `usage`, `unknown_word`, `open_quote`, `missing_token`, `unmatched`, `unclosed`, `not_top_level`, `bad_codes`, `require`, `type`, `stack_underflow` |
| 4 | `closed`: the pane waited on closed |
| 124 | `timeout`: unchanged, or the shell's `timeout` |
| 141 | stdout's reader went (`\| head -1`), as `cat`; nothing said |

For several programs, save an `ls` cursor BEFORE launch and replay journal
exits; stop on `missed`. This gives first recorded exit, never OS chronology
or group-wait order. `fux help recipes` gives the complete recipe ("First
recorded exit across jobs").

More: `fux help recipes`, `keys`, `read`, `shells`, `programs`, `servers`;
`fux help guide` gives all topics, `fux help json` the full schema,
`fux help WORD` a word's contract; `fux help cookbook` recipes to copy.

## recipes: Waiting for what you need

Each is a fuxth program: a file `fux --file FILE` runs, or `fux - <<'EOF'` in a
script. `A wait` waits from the version answer A holds, so a change between
two steps is never missed, and leaves the next answer; `A 600 timeout wait`
fails with 124 (`timeout`) if nothing changed in 600 s. A pane part way
through reading a frame (synchronized output) that has ended, the rest of
which fux already holds, keeps the answer past that time until the frame
is read, and then answers its change: a wait never says unchanged of a
pane that changed. A loop of waits ends when its check holds; to bound the
whole of it, run it under the shell's `timeout 600 fux --file FILE` (124
too). `counts` gives five integers from a live pane or the version held by
an answer, pane capture or canonical text. `A counts 3 at` is its `DONE`.

A wait's text answer is its version, then what changed (`screen`, `bell`,
`done`, `respawned`), then `exited N`, `killed by SIGNAME` (or `killed by
signal N` for an unnamed signal), or `ended, its status lost` if the program
ended. A closed pane answers `%2 closed`. A tab/workspace wait freezes its
panes as it begins and answers changes in watch order, not exit chronology.

- **Quiet for N seconds** (an agent's TUI done, or asking you something):
  `quiet` waits until nothing changes for N seconds (from a pane, an
  answer, or pane capture). A spinner or clock on the screen is a
  change, so then wait for a text.

  ```fux
  %2 str 'fix the test' paste str Enter key send-keys 5 quiet
  string
  ```
- **A text the reply writes:** count it before, act, and wait until there
  is one more. Choose a text only the reply holds: here the question holds
  no `42`. If the input itself holds it, wait until the capture shows the
  input, then count, so it is in the count, not in what you wait for; a
  program that draws the input again (a TUI that keeps it above the
  reply) counts it again. `hits` counts the lines of the last 1000 of
  history and the screen, so a count can fall as lines scroll past them: choose a window the reply
  surely comes within.

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
  %3 str 'make test' line 3 1 600 grows let finished
  finished pane str last_status get print
  finished pane last-output capture
  ```
- **A shell command's end, without marks:** print a text only its end can
  show, quoted so the line you type does not hold it, and wait for it:
  the line shows `END-OF""-RUN` as you type it, and `END-OF-RUN 0` at its
  end (`$status` in fish).

  ```fux
  %3 str 'make test; echo END-OF""-RUN $?' line
  begin 600 timeout wait %3 capture string str END-OF-RUN contains until
  drop %3 capture
  ```
- **A program's exit:** `until-ended` checks first, so an already-ended
  observation succeeds immediately, including an unknown exit status. The
  exit is told as the program is reaped, even while something it left goes
  on writing; what its terminal holds is read on, and an ended pane is
  frozen once its terminal is drained.

  ```fux
  opts str agent in 80 24 size str cargo program str test arg new-tab
  600 until-ended str exited get
  ```
- **A bell or notification:** its version's `BELLS`, `counts 2 at`, grows,
  as for a command's end.
- **A TUI's initial selection:** save as `selection.fux`; pass its pane
  as the first file argument. Here reverse video identifies the app's
  selection. That meaning belongs to this app, not to fux.

  ```fux
  args first pane
  begin
    wrapped marks capture dup string str '⟦Alpha⟧' contains not
  while changed 5 timeout wait end
  let seen seen string print seen version print
  ```
- **Change that TUI's selection:** save as `select-next.fux`; pass the
  initial capture's version. This app uses Down to select Beta. Keep the
  marked view in the loop; `shows` would use plain screens on later reads.

  ```fux
  args first expect str Down key send-keys
  begin
    wrapped marks capture dup string str '⟦Beta⟧' contains not
  while changed 5 timeout wait end
  let seen seen string print seen version print
  ```
- **What changes anywhere:** `SCOPE str CURSOR events` prints the scope's
  events after CURSOR, one JSON object a line, each with its `next`, until
  the scope is gone or the server stops, as `journalctl -f` does; an empty
  cursor starts now. Bound it with `head -n`, `timeout` or `jq`, keep it
  with `>> FILE` or `tee`, and restart from the last line's `next`.
  Take the cursor before launching jobs to observe their first recorded
  exit; group waits do not give exit chronology. Lost entries (`missed`)
  fail it on stderr: resnapshot. Events invalidate state, and do not
  replicate live OS facts. `all str CURSOR SECONDS next-events` returns one
  batch: its events, exact `next`, `missed` and status (`json`); left at
  the end, it prints its events alone, and fails on `missed`.

  ```sh
  C=$(fux all str '' 0 next-events str next get)
  # Launch jobs here, then replay from C, including exits already recorded.
  fux all str "$C" events | jq -nc 'first(inputs | select(.event == "exited"))'
  ```

  For hooks, use an ordinary program over this stream, filtering the
  triggering event and state as well as known origin. Origins identify
  supplied client/pane/program context; an external CLI is `outside`.
  No exactly-once effects are promised.
- **First recorded exit across jobs:** save this as `first-exit.py` and
  run `python3 -I first-exit.py` on the private socket. Take the cursor before
  launching, even if jobs might finish before subscription. Replay order
  is server-recorded order, not physical OS termination order. Loss is a
  failure; resnapshotting cannot recover which missing exit was first.
  Any of a, b or c may be recorded first: launch latency can change their
  order. The result includes the cursor, pane/name map and first exit record
  as an auditable result. Reads are finite batches; every CLI subprocess
  is waited for and every launched pane is cleaned up. No group wait is used.

  ```python
  def first_exit(fux):
      import json
      cursor = json.loads(fux("ls", "json"))["cursor"]
      next_cursor = cursor
      panes = {}
      try:
          for name, seconds in (("a", "0.3"), ("b", "0.05"), ("c", "0.15")):
              answer = fux("opts", "str", "agent", "in", "80", "24", "size", "str", "sh", "program",
                           "str", "-c", "arg", "str", "sleep " + seconds, "arg", "new-tab")
              panes[answer.split(":")[0]] = name
          for _ in range(256):
              batch = json.loads(fux("all", "str", next_cursor, "5", "next-events", "json"))
              if batch["missed"]:
                  raise RuntimeError("journal loss: first exit is unknown")
              for event in batch["events"]:
                  if event["event"] == "exited" and event.get("pane") in panes:
                      return {"cursor": cursor, "panes": panes, "first": event}
              next_cursor = batch["next"]
              if batch["status"] in ("gone", "stopped"):
                  raise RuntimeError("event read ended before an exit: " + batch["status"])
          raise RuntimeError("event batch limit reached before an exit")
      finally:
          for pane in panes:
              fux("str", pane, "pane", "kill")

  if __name__ == "__main__":
      import json
      import subprocess
      def fux(*words):
          command = ["timeout", "30", "fux", *words]
          return subprocess.run(command, check=True, capture_output=True,
                                text=True, timeout=30).stdout
      print(json.dumps(first_exit(fux)))
  ```
- **A deadline, a fallback, a retry:** `try` catches the failures it names
  (`programs`). A key, and C-c if nothing came of it in 10 s:

  ```fux
  %2 str y text send-keys let A
  str timeout try A 10 timeout wait drop catch drop %2 str C-c key send-keys drop end
  ```
  A command's output alone where the shell marks commands, else the screen:
  `str no_marks try %3 last-output capture catch drop %3 capture end`.
- **A shell to type into:** `shell` makes a tab with one in workspace
  `agent`, on the size of the tab `fux` runs in; from outside fux, give it
  a size: `opts 80 24 size str agent in new-tab`. Wait for its prompt
  (a text it shows) before you type: its startup may drop what comes
  sooner. A command that must run is surer started as the program: `opts str agent in
  80 24 size str sh program str -c arg str 'make test; exec
  "${SHELL:-sh}"' arg new-tab` (fux gives no `SHELL` of its own).
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux %2 all-history capture` reads
  all of it.
- **Whether a program waits for keys:** fux does not say; no system
  reports it as a fact. Wait for what the program shows when it asks (a
  prompt's text), or for quiet.
- **What a pane runs, and where:** fux gives `pid` and `foreground_group`
  (G: `%3 str foreground_group get`); the OS says the rest. Linux:
  `ps -o comm=,args= -p G`, `/proc/G/cmdline` (exact arguments),
  `readlink /proc/G/cwd`, `pgrep -a -g G` (the whole group, also once its
  leader exited); macOS: `ps -o comm=,args= -p G`, `lsof -a -p G -d cwd
  -Fn`, `pgrep -lf -g G`.

## keys: Keys, the mouse and stopping

- **Keys** go between the pane and `send-keys`: `str T text`, `str NAME
  key` (tmux's names: `Enter`, `Escape`, `Tab`, `BSpace`, `Up`, `PageDown`,
  `F5`, `C-c`, `M-s`, `S-Tab`; `Esc`, `Backspace`, `PgUp` too; a typo is
  refused; `key-names` lists the vocabulary), `str T paste` (bracketed if the program
  asked), `str HEX bytes` (`1b5b41`), of any length. A key
  N times is a loop: `%2 3 0 do str Down key loop send-keys`. A file's text, whole, from stdin (`input`):

  ```sh
  fux %2 input paste send-keys < FILE
  ```
- They are written together, in order, as the program's terminal takes
  them; an action answers once they are queued, not once the program
  read them. A program that tells a lone Escape from a key's sequence by
  time (Vim, readline) may read Escape and a key sent soon after it, in one
  `send-keys` or two, as one key: send Escape, wait until the program shows
  it took it (its mode line), then the next key. Keys sent to a program
  that exited or that closed its terminal (where the system says so:
  Linux) are refused (`not_running`). Keys a program does not read wait
  for it, as many as memory holds.

  ```fux
  %2 str Escape key send-keys
  begin 5 timeout wait %2 capture string str INSERT contains not until drop
  %2 str :wq text str Enter key send-keys
  ```
- **Keys at the screen you saw:** `SEEN expect` makes keys for SEEN's
  pane that `send-keys` sends only if the pane is still at version SEEN
  as they are queued; else it sends none and fails `stale`. SEEN is a
  pane's capture (its version as fux read the screen, as its `json`'s
  `version` says), an action's or wait's answer, or a version as text.
  Every count of the version (above) is a change. That is all it
  promises: the pane was where you saw it when the keys were queued, not
  that its program has not moved on before it reads them, nor what output
  is on its way that fux has not read. A pane closed since fails
  `no_pane` instead (a shell pane closes as its shell exits; a pane
  started with a program stays, and fails `stale`); a version of another server
  run, or with counts the pane can't have had, fails `bad_value`, as for
  `changed`. A version is one pane's: to a
  tab's panes, send each its own; keys to a client go to the pane the
  client chooses, and take no version.

  ```fux
  %2 capture let seen
  seen expect str y text str Enter key send-keys   # stale if %2 changed since
  ```
- **Mouse:** `%2 str click ROW COL send-mouse` (also `right`,
  `middle`, `press`, `release`, `wheel-up`, `wheel-down`, `move`), cells
  from 0 as `wrapped capture` counts them; on a double-width row, click at
  2×COL. A program's own "row 22" is usually counted from 1.
- **Stop:** `%2 str TERM signal` (`INT`, `KILL`, …) signals what runs in
  its terminal's foreground; `%2 kill` closes the pane; `%2 respawn` starts
  the pane's program again once it ended (`kill-first` ends it first; a
  shell's pane is refused, `not_run`). The new program starts at once, and
  takes over once what the old one left is read, in turns as any output is,
  and its exit told. Meanwhile the old program is let go and the pane runs
  none of its own: input is refused (`not_running`), as are `signal` and
  another respawn (`still_running`); closing the pane ends the new program,
  and the respawn answers `closed`. The respawn goes on if its caller goes.

## read: Reading the screen

For metadata, take one `snapshot`: `str agent workspace str topology snapshot`
holds the workspace's ordered tabs, panes, tree and client placement. Its
`str panes get` is a local list of records; each record's `str id get` is a
handle and `str title get` is the retained title. Use `json` to read the flat
collections in another language. Topology makes no process metadata queries;
`facts` adds process/terminal state and `recipe` adds saved launch inputs.

`PANE READ capture`, READ any of: `100 history`, `all-history`,
`main-screen` (what was printed before a program took the screen),
`alt-screen` (the full screen a program drew last), `last-output` (the
last shell command's output, or the running one's so far, between its
marks: what a clear erased there reads blank, and an ended command's
last row ends at its end mark, so a last line ending in `\r` reads
empty), `wrapped` (each screen row a line; else a line wrapped at the
pane's width is joined whole), `0 4 rows`, `0 40 cols`, `full`, `marks`
(reverse video and any background colour as `⟦this⟧`: a program that
paints its whole background is marked whole), `styles`. Blanks at a
line's end are trimmed, so a prompt's `>>> ` reads `>>>`; `full` keeps
every cell. Then `json` for the screen and what is known of the pane:
`screen_cursor` is `[ROW,COL]` (`screen_rows` and `screen_cols` the
screen's size), `alt_screen` whether a program holds the
alternate screen, `history_lines` the history kept, `above` the history
rows kept above the first row read. Rows and columns are half-open
`start..end` ranges from 0, like `slice`; equal endpoints select nothing.
A joined row range includes each whole logical line it touches; `wrapped`
selects physical rows only.
Every pane of a tab or workspace, in layout order: `str agent workspace
panes each capture print end`; `tabs` gives its tabs the same way.

## shells: Shells and their marks

`fux opts 80 24 size str agent in new-tab` gives a shell (run in a fux
pane, the size may go: the tab is then laid out on that pane's tab). fux starts shells as they
are; a shell that sends OSC 133 marks (`C` as a command's output begins,
`D;STATUS` as it ends, `A` at a prompt) counts the commands that end in
its pane's version (`recipes`), gives `last_status`, lets `last-output`
read a command's output alone. fux reads marks from any program that sends them. Without marks
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
for its exit: `fux opts str agent in 80 24 size str ssh program str -n arg str HOST arg
str make arg new-tab`.

## programs: A program

Use `require profiles/agent.fux` to explicitly load shared definitions or
policy before the whole program is checked. Paths are literal, relative to
the requiring file or the invocation cwd for command-line/stdin roots.
Canonical paths load once; cycles fail. No user config is imported implicitly
by a CLI program. Only regular files are admitted.

A file of fuxth runs with `fux --file FILE ARGS…` (or `fux -` from stdin;
a script may start `#!/usr/bin/env -S fux --file`), and keeps its names
between steps:

```fux
# test.fux: run the tests, wait until they end, read what they printed
opts str agent in str tests name 80 24 size str cargo program str test arg new-tab   # its answer: %2:0.0.0.0.0:RUN
begin 600 timeout wait dup counts 4 at 1 eq until  # ENDED, its last count, is 1
all-history capture
```

`name` there names the tab (`str tests tab`). A pane is named by a second
request on its answer or handle: `new-tab dup str tests rename`, then
`str tests pane`.

`X let NAME` names a value, `define NAME … end` makes a word, `if … else …
then` and `begin … until` decide and repeat (`begin … FLAG while … end`
tests first; `LIMIT FIRST do … loop` counts, `i` its index), `list … end`
gathers what its body leaves, `SEQ each … end` runs once per element, `CODES try … catch …
end` runs its handler instead of failing with one of CODES (`str timeout
try %2 5 timeout wait catch … end`, `str all try` for any: its body
starts with an empty stack; the handler, with the stack as at `try` and
one failure on it, whose fields `get` reads: `code`, `number` (in
Forth's table, or null), `message` (the word's own, with no place, word
or usage added), `value` (a failed wait's answer, or null) and `actions`;
`throw` raises it again); a failure it does
not name goes on, its status kept. An action leaves its answer: printed,
it is the lines above; given to a word that wants a pane, it is its pane (an action's or wait's on a tab or workspace
is of another kind, `answers`, which no such word takes, however many panes
it has: name the tab again, `@1 wait`), and to
`wait`, the version it holds too (`T str y text send-keys wait` waits from
before the keys; a name keeps that version, so `T wait` later waits from
when `T` was made, and `T str version get changed wait` from the same held version).

Modifiers retain whether a request names a pane, tab, workspace or client.
`changed` makes a `changed-request`, consumed only by `wait`; `expect`
makes an `expected-request`, consumed only by `send-keys`. A pane capture
has kind `capture` and a version; a rendered client screen has kind
`client-capture` and none. A caught closed pane has kind `closed-answer`:
it can name its pane (which is gone), but supplies no version to `changed`
or `expect`. A handler may also receive an ordinary message string, so
uncertain values still need runtime checks.

`string` converts a completed value to text (`%2 capture string str error
contains`). A request with unused options must first reach its action; converting
it with `string` does not complete it. Unknown words fail with the fixed hint
`text is str WORD`; fux does not infer an executable name or a correction. The
standard library, written in fuxth, does the rest (`fux help` lists it).
Words run as they are read, so the words before a failing one have
already acted: stderr lists each step that took effect, once, after why
it failed (`fux: already took effect: argument 4: rename`), and a
caught failure holds them as `actions`. A run exits 0, what is left
printed; or the one `N exit` gives (0 to 255); or with the status of the
failure that stopped it (above), stdout getting nothing more and stderr
why, then what was left.
`str NAME getenv` reads a local variable in config, a binding or CLI mode; an unset
variable fails `unset_var`, never an empty string. From the command line
alone, a script reads `args` (the strings after `--file FILE`), `input`
(the rest of stdin; not when the program came from it), and `str PATH
read-file`. `X warn` prints on stderr:
`args len 0 eq if str 'usage: job.fux PANE' warn 2 exit then`.

## servers: Servers, sizes and more

- A pane's program takes the server's environment, plus `FUX`,
  `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER`. `env(1)` sets the rest:
  `fux opts str agent in 80 24 size str env program str PORT=8080 arg str ./app arg new-tab`.
- Handle values returned by a server retain its run through lists, bindings,
  answers and request options; using one against another run is refused.
  A hand-written `%N`, `@N`, `+N` or `str cN client` means the server
  `FUX_SOCKET` names, now. Printing one of these handles loses its run: that text is not durable
  across restarts.
- `FUX_SOCKET=PATH fux …` talks to the server on that socket (an absolute
  path), else `server.sock` in fux's directory; `start` starts one, with
  nothing in it (the first client to attach starts a shell on its screen;
  one that only watches is refused, as there is nothing to watch), `status`
  says which answers, and fails (`no_server`) where none does: `fux status
  || fux start`. One `fux` command talks to one server; join two with a
  pipe: `fux %1 capture | FUX_SOCKET="$XDG_RUNTIME_DIR/fux/b.sock" fux %1
  input paste send-keys`. The servers in fux's directory (`$XDG_RUNTIME_DIR/fux` whenever
  `XDG_RUNTIME_DIR` is set, else `$TMPDIR/fux`) are its `.sock` files:
  `status` each.

  ```sh
  for s in "${XDG_RUNTIME_DIR-$TMPDIR}"/fux/*.sock; do FUX_SOCKET=$s fux status; done
  ```
  Inside a person's pane, `here` is that pane. Every pane has
  `FUX_PANE` (`%N:GENERATION`, the pane and its program generation),
  `FUX_SOCKET` (its server's socket), `FUX_SERVER` (which run of the server:
  a `FUX_PANE` of another, or with no `FUX_SERVER`, is no `here`:
  `stray_pane`; neither is a caller from before a respawn) and `FUX`
  (the fux the server runs, for a script that may not find `fux` on PATH;
  unset if the server cannot name it). Resolve `FUX_PANE` with `here`;
  its text before the colon alone is the pane id. A person
  who wants your panes apart from theirs gives you a server of your own
  (`FUX_SOCKET=PATH fux start`), which they watch by attaching to it.
  `kill-server` from inside its own server is refused;
  `kill` on its process stops it: `kill "$(fux status str server_pid get)"`,
  or on macOS, which says no `server_pid`, the pid `lsof -t SOCKET` gives.
  A command line against a server of another
  fux protocol runs again with that server's own binary, saying so in one
  line on stderr; where that cannot be, it fails with `mismatch`, saying why.
- Nothing is saved on its own: a server that stops, or crashes, takes its
  session with it. `fux all dump > FILE` saves its layout and launches when you
  choose; `fux start && fux --file FILE` makes them again on a fresh server,
  never its processes' state. That is another server run: versions from
  before it are refused.
- fux is controlled through its Unix socket alone: a sandbox that denies
  `connect` to it (Codex's denies every `connect`) cannot drive fux. Run
  fux commands outside such a sandbox, or allow the socket.
- `@2 attach` shows that tab's first pane; `%3 attach` focuses that pane.
  `opts str work in attach` requires that workspace to exist. Without a target,
  attach uses the first stored workspace; a writable attach carries its launch,
  which only an empty server starts, serialized by the server. One viewport
  supplies the pane size (less a bar row, if its bar has one). Watchers never create.
  `opts str work in attach-defaults` is a local preference; explicit attach
  operands win. A fresh typing client loads config edits; existing clients
  keep their frozen configuration.
- A tab no client shows, or only clients that watch, is laid out on its
  `size`; `TAB 40 12 resize` changes it, `resize-auto` gives it back to a
  client's screen, `PANE 95 percent str horizontal resize-pane` sizes a pane in its tab. A
  size that cannot be had is refused (`size`, as is a screen past what a
  grid holds, from `COLS ROWS size` or the server), and nothing changes; so is a split
  or a move that would hide a pane on the tab's own size or on a screen
  showing it (but one that sizes nothing: that only watches, or has no
  room below a bar row).
- To keep what a program writes, run it under `script` in its pane, so it
  still writes to a terminal (a pipe, as with `tee`, changes what it
  writes): `opts str agent in str script program str -q arg str -e arg str -f arg str
  LOG arg str -c arg str 'make test' arg new-tab` (util-linux; on macOS and
  the BSDs, `str -q arg str -F arg str LOG arg str sh arg str -c arg str
  'make test' arg`). LOG holds the terminal's raw bytes (util-linux adds a
  first and last line of its own). `all dump` prints the layout as a program
  that makes it again; `opts 100 30 size attach` is a client with no
  terminal (`str c2 client … send-keys` types at it, `capture`
  shows it): how to drive fux's own screens.

A split needs an explicit axis: `%2 str horizontal axis split`; resize
with `%2 50 percent str horizontal resize-pane`. `panes` and `tabs`
read ordered membership; `ls` reads one facts snapshot.
