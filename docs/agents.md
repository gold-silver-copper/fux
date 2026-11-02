# Driving programs with fux

fux runs programs in terminals (panes) on a server, and tells you what
they show and how they end, as facts: never whether one is ready or done.
`fux help cookbook` has a tested recipe for each task below; `fux help
WORD` says one word, `fux help json` every field.

**A server of your own.** Every `fux` command talks to the server
`FUX_SOCKET` names; a fux pane sets it to its own server's. Beside a
person, use your own, so your panes and theirs stay apart: a socket in a
directory only you may open (export it in each later shell), started:

```sh
export FUX_SOCKET="$(mktemp -d)/fux.sock"
fux status > /dev/null 2>&1 || fux start
```

**Words.** A command line is a fuxth program, its words run left to right
on a stack: `str WORD` pushes text, `%2` `@2` `+1` are a pane, tab and
workspace, `str NAME pane` a pane by name. What is left prints, a line each.

**A tab of your own.** From outside fux a new tab needs a workspace and
a size (from a fux pane, it takes that pane's tab's). `in` names the
workspace, made if it isn't there; `in` again nests one in it (`str proj
in str agent1 in`). `new-tab` answers its pane's version
(`%3:0.0.0.0.0:RUN`); here `rename` names the pane instead:

```sh
fux opts str agent in 80 24 size str sh program new-tab str sh1 rename
```

**Drive it, and wait.** An action (`line`, `send-keys`, `signal`) answers
at once, before the program has read anything. A wait answers once the
pane changes: `shows` waits for a text (one only the output holds: the
line typed shows `$((6*7))`, never `42`), `quiet` for N seconds of no
change (a fact of the screen, not proof of an end), `until-ended` for
the program's exit; each leaves what it saw. Their seconds bound each
change, not the whole: `timeout 600 fux …` bounds a command (exit 124).
Keys: `str C-c key send-keys` (`fux key-names` names them). To act only on
the screen you read, give its version: `expect` sends nothing and fails
`stale` if the pane changed since. A file's text goes as one paste.

```sh
fux str sh1 pane str 'sleep 1; echo "built: $((6*7))"' line
fux str sh1 pane str 'built: 42' 60 shows
fux str sh1 pane 2 quiet
fux opts str jobs in 80 24 size str sh program str -c arg str 'exit 3' arg new-tab 60 until-ended str program get str exited get
V=$(fux str sh1 pane capture version)
fux str "$V" expect str 'echo yes' line
printf 'echo pasted\n' > lines.txt
fux str sh1 pane input paste send-keys < lines.txt
```

**Read it in one observation.** `capture` gives the screen as text,
`100 history capture` with history, `last-output capture` the last shell
command's output (`fux help shells`). `capture json` is the pane's whole
observation at one version: `lines`, `version`, `cursor`, `program` (`pid`,
`state`, `exited`, `signal`, `foreground_group`), `last_status` and more.

**Follow what changes.** A capture's `cursor` is the journal's place at
that screen. `events` from it prints each later event of the pane (or of
`all`), one JSON object a line with its `next`, until the scope is gone;
`head` (141 then), `jq` or `timeout` ends it, `>> FILE` keeps it. An entry
lost before it was read fails it (`missed`): capture again. A screen's
change is no event: it shows as a new version.

```sh
fux str sh1 pane capture json | jq -c '{version, program, last_status}'
C=$(fux str sh1 pane capture str cursor get)
fux str sh1 pane str 'echo after' line
fux str sh1 pane str "$C" events | head -n 1 || [ $? -eq 141 ]
```

**Leave the OS's work to the OS.** fux has no words for these:

| For | Use |
| --- | --- |
| a program's environment | `env(1)`: `str env program str PORT=8080 arg str ./app arg new-tab` |
| a time limit | `timeout(1)`, on fux or in a pane: `str timeout program str 60 arg …` |
| a file's text | `< FILE`, read by `input` |
| what a pane runs, and where | its `program`'s `foreground_group` G: `ps -o comm=,args= -p G`; Linux `readlink /proc/G/cwd`, `pgrep -a -g G`; macOS `lsof -a -p G -d cwd -Fn`, `pgrep -lf -g G` |
| a log of what a program wrote | `script(1)` in its pane (`fux help servers`) |

**Stop.** Close your workspace and all in it; kill your `events` readers
by PID. A server ends with its last pane; one still up stops with `kill`
on its process, which macOS leaves out of `status`, where `lsof -t` names it:

```sh
fux str agent workspace kill
if fux status > /dev/null 2>&1; then kill "$(fux status str server_pid get 2>/dev/null || lsof -t "$FUX_SOCKET")"; fi
```

fux exits 0 when done (or with what `N exit` gave); 1 when refused, saying
`fux: CODE: MESSAGE` on stderr; 2 for a mistake in the program (`usage`,
`unknown_word`, `type` …); 4 if the pane waited on closed; 124 on
`timeout`; 141, saying nothing, when stdout's reader went, as `cat` does.

More: `fux help TOPIC` for `recipes`, `typing`, `read`, `shells`,
`programs` and `servers`; `fux help guide` all of them; `fux help cookbook`.

## recipes: Waiting for what you need

Each is a fuxth program: a file `fux --file FILE` runs, or `fux -` reads
from stdin. A loop of waits, a guard on a version, a fallback: what must
happen inside one program. The shell's recipes are `fux help cookbook`'s.

`A wait` waits from the version answer A holds, so a change between two
steps is never missed, and leaves the next answer; `A 600 timeout wait`
fails with 124 (`timeout`) if nothing changed in 600 s. A pane part way
through a frame (synchronized output) whose rest fux already holds keeps
the answer until the frame is read, then answers its change: a wait never
says unchanged of a pane that changed. To bound a whole loop of waits,
run it under `timeout 600 fux --file FILE` (124 too). `counts` gives five
integers, RESPAWNS SCREEN BELLS DONE ENDED, from a live pane or the
version an answer, capture or text holds: `A counts 3 at` is its `DONE`.

A wait's text answer is its version, then what changed (`screen`, `bell`,
`done`, `respawned`), then `exited N`, `killed by SIGNAME` (or `killed by
signal N` for an unnamed signal), or `ended, its status lost` if the program
ended. A closed pane answers `%2 closed`. A tab/workspace wait freezes its
panes as it begins and answers changes in watch order, not exit chronology.

`shows` checks text presence; old text and input echo can match. Use a
reply-only result (42 is absent from 6*7), shell end marks, or a
TUI-specific predicate. `until-ended` checks ENDED before waiting, so an
already-ended program succeeds at once, and follows the pane if respawned.
A capture keeps screen, version and metadata together; `get` and `version`
on it read locally. `observed` retains a pane capture or samples a pane or
single answer. Exit status and signal are distinct; an unknown status
stays unknown.

- **Launch and wait for a prompt.** Run with `timeout 30 fux -` and this
  program on stdin; a banner alone is not the prompt:

  ```fux
  opts str agent in 80 24 size str python3 program str -q arg new-tab
  str '>>>' 5 shows let seen
  seen string print seen version print
  ```
- **Run a command, read its output and actual exit status.**
  Run with `timeout 30 fux -` and this program on stdin:

  ```fux
  opts str agent in 80 24 size str sh program str -c arg
  str 'echo result=42; exit 7' arg new-tab
  10 until-ended let seen
  seen string print seen str program get str exited get print
  ```
- **Guard input against the screen you read.** Save this as `reply.fux`
  and run `timeout 30 fux --file reply.fux "$V"`, with the prompt's
  version above:

  ```fux
  args first expect str '6*7' text str Enter key send-keys
  str 42 5 shows let seen
  seen string print seen version print
  ```
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
  600 until-ended str program get str exited get
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
- **What changes anywhere:** the page's `events`, and the cookbook's
  "Follow events from a cursor" and "Keep a journal file". A hook is an
  ordinary program over that stream, filtering the triggering event and
  state as well as its origin (an outside command line is `outside`); no
  exactly-once effects are promised. `all str CURSOR SECONDS next-events`
  returns one batch instead: its `events`, the oldest entry still held
  (`first`), exact `next`, `missed` and `status`.
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
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux %2 all-history capture` reads
  all of it.
- **Whether a program waits for keys:** fux does not say; no system
  reports it as a fact. Wait for what the program shows when it asks (a
  prompt's text), or for quiet.

## typing: Keys, the mouse and stopping

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
`facts` gives each pane's whole observation, its program, launch and
terminal state with it, as `PANE capture json` does, and `recipe` adds
where each program is now.

`PANE READ capture`, READ any of: `100 history`, `all-history`,
`main-screen` (what was printed before a program took the screen),
`alt-screen` (the full screen a program drew last), `last-output` (the
last shell command's output, or the running one's so far, between its
marks: what a clear erased there reads blank, and an ended command's
last row ends at its end mark, so a last line ending in `\r` reads
empty), `wrapped` (each screen row a line; else a line wrapped at the
pane's width is joined whole), `0 4 rows`, `0 40 cols`, `full`, `marks`
(reverse video and any background colour as `⟦this⟧`: a program that
paints its whole background is marked whole; text rendered faint as
`⟨this⟩`), `styles`, `str VERSION since` (only the lines with a row
changed since that version, each written after its row, `3:text`;
`stale` where fux can't say which: after a scroll, resize or respawn).
Blanks at a line's end are trimmed, so a prompt's `>>> ` reads `>>>`;
`full` keeps every cell. Then `json` for the screen and the pane's whole
observation, its fields as `ls` gives them read in the same turn:
`screen_cursor` is `[ROW,COL]` (`rows` and `cols` the
screen's size), `alt_screen` whether a program holds the
alternate screen, `history_lines` the history kept, `above` the history
rows kept above the first row read, `wrapped_rows` which rows go on in
the next, `links` the hyperlinks (OSC 8) as `row`, `col`, `len` and `uri`,
`last_output` where `last-output` reads, `cursor` the journal's place
read with it (`events` from it). `fux help json` says every field. Rows and columns are half-open
`start..end` ranges from 0, like `slice`; equal endpoints select nothing.
A joined row range includes each whole logical line it touches; `wrapped`
selects physical rows only.
Every pane of a tab, a workspace (with the workspaces in it) or `all`, in
layout order: `str agent workspace panes each capture print end`; `tabs`
gives its tabs the same way.

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

`require profiles/agent.fux` reads a file's words where it stands, once
per file, relative to the requiring file (or the cwd, for the command
line and stdin); `include` reads one each time. A source is any file that
reads, a FIFO or `<(…)` among them; a cycle fails. No config is loaded
by a command line unless it requires it.

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
request on its answer or handle: `new-tab dup str runner rename`, then
`str runner pane`. A name is one thing's: a workspace, tab or pane that
has it already refuses it to another (`name_taken`).

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
  A command whose reply was lost has an unknown outcome: the server may
  have made a process or queued its input, so running it again is no way
  to learn whether it ran.
- Nothing is saved on its own: a server that stops, or crashes, takes its
  session with it. `fux all dump > FILE` saves its layout and launches when you
  choose; `fux start && fux --file FILE` makes them again on a fresh server,
  never its processes' state. That is another server run: versions from
  before it are refused.
- fux is controlled through its Unix socket alone: a sandbox that denies
  `connect` to it (Codex's denies every `connect`) cannot drive fux. Run
  fux commands outside such a sandbox, or allow the socket.
- `@2 attach` shows that tab's first pane (its zoomed one, if zoomed); `%3 attach` focuses that pane.
  `opts str work in attach` requires that workspace to exist. Without a target,
  attach uses the first stored workspace; a writable attach carries its launch,
  which only an empty server starts, serialized by the server. One viewport
  supplies the pane size (less a bar row, if its bar has one). Watchers never create.
  `opts str work in attach-defaults` is a local preference; explicit attach
  operands win. A fresh typing client loads config edits; existing clients
  keep their frozen configuration.
- A tab is laid out once, on its effective size: one given a `size`
  (`TAB 40 12 resize`) always on that; else (`resize-auto`) on the smallest
  screen of the clients showing it, watchers left out, while every pane fits there, else on the last size that did.
  `PANE 95 percent str horizontal resize-pane` sizes a pane in its tab; its `row` and `col` say where it is. A
  size that cannot be had is refused (`size`, as is a screen past what a
  grid holds, from `COLS ROWS size` or the server), and nothing changes; so is a split
  or a move that would hide a pane on that size. A larger screen shows the
  tab at the top left, the rest blank.
- To keep what a program writes, run it under `script` in its pane, so it
  still writes to a terminal (a pipe, as with `tee`, changes what it
  writes): `opts str agent in str script program str -q arg str -e arg str -f arg str
  LOG arg str -c arg str 'make test' arg new-tab` (util-linux; on macOS and
  the BSDs, `str -q arg str -F arg str LOG arg str sh arg str -c arg str
  'make test' arg`). LOG holds the terminal's raw bytes (util-linux adds a
  first and last line of its own). `all dump` prints the layout as a program
  that makes it again; `opts 100 30 size attach` is a client with no
  terminal (`str c2 client … send-keys` types at it, `capture`
  shows it): how to drive fux's own screens. Each client is shown its own
  workspace, tab and pane: `str c2 client %3 show` shows it `%3`, and
  `str c2 client @2 str next step` the tab after `@2` (`str last`: the one
  it was shown there before; a pane also steps `str left` and so on).

A split needs an explicit axis: `%2 str horizontal axis split`; resize
with `%2 50 percent str horizontal resize-pane`. `panes` and `tabs`
read ordered membership; `ls` reads one facts snapshot.
