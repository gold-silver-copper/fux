# Driving programs with fux

Use your assigned private `FUX_SOCKET`. Beside a human, use a private
socket, `XDG_RUNTIME_DIR` (mode 0700) and `XDG_CONFIG_HOME`; a workspace
alone is not isolation. `fux start` starts an empty server.

Words run left to right. `str WORD` pushes text; %2 is a pane, @2 a tab,
+1 a workspace. Options precede the action. Remaining values print.
Outside fux, give new tabs a size. `run` requires a workspace;
`agent-opts` supplies `opts str agent in`.

**Launch and wait for a prompt.** Run with `timeout 30 fux -` and this
program on stdin; a banner alone is not the prompt:

```fux
agent-opts 80 24 size str python3 program str -q arg run
str '>>>' 5 shows let seen
seen string print seen version print
```

**Run a command, read its output and actual exit status.**

```fux
agent-opts 80 24 size str sh program str -c arg
str 'echo result=42; exit 7' arg run
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
CLI status: 0 success; 1 failure (`fux: CODE: MESSAGE`); 4 closed;
124 unchanged or outer timeout. A transport failure may leave input queued.

For several programs, save an `ls` cursor BEFORE launch and replay journal
exits; stop on `missed`. This gives first recorded exit, never OS chronology
or group-wait order. The complete recipe is below.

More: `fux help recipes`, `keys`, `read`, `shells`, `programs`, `servers`;
`fux help guide` gives all topics, `fux help json` the full schema,
`fux help WORD` a word's contract.

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
  observation succeeds immediately, including an unknown exit status.

  ```fux
  agent-opts 80 24 size str cargo program str test arg run
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
    wrapped marks capture dup string str '⟦Alpha⟧' contains
    if true else changed 5 timeout wait false then
  until
  let seen seen string print seen version print
  ```
- **Change that TUI's selection:** save as `select-next.fux`; pass the
  initial capture's version. This app uses Down to select Beta. Keep the
  marked view in the loop; `shows` would use plain screens on later reads.

  ```fux
  args first expect str Down key send-keys
  begin
    wrapped marks capture dup string str '⟦Beta⟧' contains
    if true else changed 5 timeout wait false then
  until
  let seen seen string print seen version print
  ```
- **What changes anywhere,** as it happens: `fux all events` streams a JSON
  line for each event (`fux help json` lists them). To read the session,
  then follow its events without missing one in between, start the stream
  from the cursor `ls json` gave with the listing (its `events`); a
  `missed` line says the journal let some go first: read `ls` again.

  ```sh
  C=$(fux ls json | jq -r .events)
  fux all str "$C" from events      # or: fux %3 str "$C" from events
  ```
- **First recorded exit across jobs:** save this as `first-exit.py` and
  run `python3 first-exit.py` on the private socket. Take the cursor before
  launching, even if jobs might finish before subscription. Replay order
  is server-recorded order, not physical OS termination order. Loss is a
  failure; resnapshotting cannot recover which missing exit was first.
  The subscriber is always terminated and reaped. No group wait is used.

  ```python
  def first_exit(fux):
      import json
      import subprocess
      cursor = json.loads(fux("ls", "json"))["events"]
      panes = {}
      try:
          for name, seconds in (("a", "0.3"), ("b", "0.05"), ("c", "0.15")):
              answer = fux("agent-opts", "80", "24", "size", "str", "sh", "program",
                           "str", "-c", "arg", "str", "sleep " + seconds, "arg", "run")
              panes[answer.split(":")[0]] = name
          stream = fux("all", "str", cursor, "from", "events", stream=True)
          try:
              for line in stream.stdout:
                  event = json.loads(line)
                  if event["event"] == "missed":
                      raise RuntimeError("journal loss: first exit is unknown")
                  if event["event"] == "exited" and event.get("pane") in panes:
                      return panes[event["pane"]]
              raise RuntimeError("event stream ended before an exit")
          finally:
              stream.terminate()
              try:
                  stream.wait(timeout=5)
              except subprocess.TimeoutExpired:
                  stream.kill()
                  stream.wait()
              stream.stdout.close()
      finally:
          for pane in panes:
              fux("str", pane, "pane", "kill")


  if __name__ == "__main__":
      import subprocess
      def fux(*words, stream=False):
          command = ["timeout", "30", "fux", *words]
          if stream:
              return subprocess.Popen(command, stdout=subprocess.PIPE, text=True)
          return subprocess.run(command, check=True, capture_output=True,
                                text=True, timeout=30).stdout
      print(first_exit(fux))
  ```
- **A deadline, a fallback, a retry:** `try` catches the failures it names
  (`programs`). A key, and C-c if nothing came of it in 10 s:

  ```fux
  %2 str y text send-keys let A
  str timeout try A 10 timeout wait drop catch 2drop %2 str C-c key send-keys drop end
  ```
  A command's output alone where the shell marks commands, else the screen:
  `str no_marks try %3 last-output capture catch 2drop %3 capture end`.
- **A shell to type into:** `shell` makes a tab with one in workspace
  `agent`, on the size of the tab `fux` runs in; from outside fux, give it
  a size: `opts 80 24 size str agent in new-tab`. Wait for its prompt
  (a text it shows) before you type: its startup may drop what comes
  sooner. A command that must run is surer started as the program: `agent-opts
  80 24 size str sh program str -c arg str 'make test; exec
  "${SHELL:-sh}"' arg run` (fux gives no `SHELL` of its own).
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
  refused; `key-names` lists the vocabulary), `str T paste` (bracketed if the program
  asked), `str HEX bytes` (`1b5b41`). 900 KiB at most in all. A key
  N times is a loop: `%2 0 3 range each drop str Down key end
  send-keys`. fuxth reads no files; the shell gives a file's text:

  ```sh
  fux %2 str "$(cat FILE)" paste send-keys   # drops its last line ends; one argument's size at most (Linux: 32 memory pages)
  { printf "%s str '" %2; sed "s/'/'\\\\''/g" FILE; printf "' paste send-keys"; } | fux -   # all of it, send-keys' 900 KiB at most, quoted for fuxth
  ```
- They are written together, in order, as the program's terminal takes
  them; an action answers once they are queued, not once the program
  read them. A program that tells a lone Escape from a key's sequence by
  time (Vim, readline) may read Escape and a key sent soon after it, in one
  `send-keys` or two, as one key: send Escape, wait until the program shows
  it took it (its mode line), then the next key. Keys sent to a program
  that exited or that closed its terminal (where the system says so:
  Linux) are refused (`not_running`), and keys past what the pane's input
  queue holds (1,048,768 bytes its program has not read) are refused whole
  (`input_full`; more than the queue ever holds, `too_long`).

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
  `run` started stays, and fails `stale`); a version of another server
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
for its exit: `fux agent-opts 80 24 size str ssh program str -n arg str HOST arg
str make arg run`.

## programs: A program

Use `require profiles/agent.fux` to explicitly load shared definitions or
policy before the whole program is checked. Paths are literal, relative to
the requiring file or the invocation cwd for command-line/stdin roots.
Canonical paths load once; cycles fail. No user config is imported implicitly
by a CLI program. The source graph is capped at 16 MiB, 256 files, 32 require
levels and 1,000,000 tokens; only regular files are admitted.

A file of fuxth runs with `fux --file FILE ARGS…` (or `fux -` from stdin;
a script may start `#!/usr/bin/env -S fux --file`), and keeps its names
between steps:

```fux
# test.fux: run the tests, wait until they end, read what they printed
agent-opts str tests name 80 24 size str cargo program str test arg run   # its answer: %2:0.0.0.0.0:RUN
begin 600 timeout wait dup counts 4 at 1 eq until  # ENDED, its last count, is 1
all-history capture
```

`X let NAME` names a value, `define NAME … end` makes a word, `if … else …
then` and `begin … until` decide and repeat, `list … end` gathers what its
body leaves, `SEQ each … end` runs once per element, `CODES try … catch …
end` runs its handler instead of failing with one of CODES (`str timeout
try %2 5 timeout wait catch … end`: its body starts with an empty stack;
the handler, with the stack as at `try` and the failure's own message,
with no place, word or usage added, or a failed wait's answer, and code
on it); a failure it does
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
standard library, written in fuxth, does the rest (`fux help` lists it). A
mistake every run would make (too few values, a string given to `add` or
`if`, a request nothing acts on: its error names the word to end it with)
fails before any word acts; otherwise the words before a failing one have
already acted. A run exits 0, or with the status of the failure that
stopped it, or the one `N exit` gives (0 to 255), what is left printed.
`str NAME getenv` reads a local variable in config or CLI mode; an unset
variable fails `unset_var`, never an empty string. From the command line
alone, a script reads `args` (the strings after `--file FILE`), `input`
(the rest of stdin; not when the program came from it), and `str PATH
read-file`. `X warn` prints on stderr:
`args len 0 eq if str 'usage: job.fux PANE' warn 2 exit then`.

## servers: Servers, sizes and more

- `FUX`, `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER` are reserved pane
  identity. Requesting them is refused: `bad_value` in `env`, `config` in
  `copy-environment`. Other names, including `FUX_EXTRA`, remain allowed.
- Handle values returned by a server retain its run through lists, bindings,
  answers and request options; using one against another run is refused.
  A hand-written `%N`, `@N`, `+N` or `str cN client` means the selected server
  now. Printing one of these handles loses its run: that text is not durable
  across restarts.
- `str NAME server` (`NAME.sock` in fux's directory) or `str PATH socket`
  first chooses another server; `start` starts one, with nothing in it
  (the first client to attach starts a shell on its screen; one that only
  watches is refused, as there is nothing to watch), `status` says which
  answers, and fails (`no_server`) where none does: `fux status || fux
  start`. The servers in fux's directory (`$XDG_RUNTIME_DIR/fux` whenever
  `XDG_RUNTIME_DIR` is set, else `$TMPDIR/fux`) are its `NAME.sock` files:
  `status` each.

  ```sh
  ls "${XDG_RUNTIME_DIR-$TMPDIR}/fux" | sed -n -e 's/\.sock$//p' |
    while read -r n; do fux str "$n" server status; done
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
  `force-kill-server` stops it.
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
  attach uses the first stored workspace; only a writable empty attach retries
  with conditional initial creation, serialized by the server. One viewport
  supplies the pane size after subtracting the bar. Watchers never create.
  `opts str work in attach-defaults` is a local preference; explicit attach
  operands win. A fresh typing client loads config edits; existing clients
  keep their frozen configuration.
- A tab no client shows, or only clients that watch, is laid out on its
  `size`; `TAB 40 12 resize` changes it, `resize-auto` gives it back to a
  client's screen, `PANE 95 percent str horizontal resize-pane` sizes a pane in its tab. A
  size that cannot be had is refused, and nothing changes; so is a split
  or a move that would hide a pane on the tab's own size or on a screen
  showing it (but one that sizes nothing: that only watches, or has no
  room below its bar).
- To keep what a program writes, run it under `script` in its pane, so it
  still writes to a terminal (a pipe, as with `tee`, changes what it
  writes): `agent-opts str script program str -q arg str -e arg str -f arg str
  LOG arg str -c arg str 'make test' arg run` (util-linux; on macOS and
  the BSDs, `str -q arg str -F arg str LOG arg str sh arg str -c arg str
  'make test' arg`). LOG holds the terminal's raw bytes (util-linux adds a
  first and last line of its own). `all dump` prints the layout as a program
  that makes it again; `opts 100 30 size attach` is a client with no
  terminal (`str c2 client … send-keys` types at it, `capture`
  shows it): how to drive fux's own screens.

A split needs an explicit axis: `%2 str horizontal axis split`; resize
with `%2 50 percent str horizontal resize-pane`. `panes` and `tabs`
read ordered membership; `ls` reads one facts snapshot.
