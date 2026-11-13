# Driving programs with fux

fux runs programs in terminals (panes) on a server, and tells you what
they show and how they end, as facts: never whether one is ready or done.
`fux '(explain "cookbook")'` has a tested recipe for each task below; `fux
'(explain "split")'` says one operation, `docs/records.md` every field.

**A server of your own.** Every `fux` command talks to the server
`FUX_SOCKET` names; a fux pane sets it to its own server's. Beside a
person, use your own, so your panes and theirs stay apart: a socket in a
directory only you may open (export it in each later shell), started:

```sh
export FUX_SOCKET="$(mktemp -d)/fux.sock"
fux '(start)'
```

**Programs.** A command line is ONE quoted grift program: `(pane 2)` `(tab 2)`
`(workspace 1)` are a pane, tab and workspace, `(pane "NAME")` a pane by name. The
last form's value prints, a final list an item a line. Shell arguments after it
stay strings, read with `(arguments)`: pass text that way, never pasted in.

**A tab of your own.** From outside fux a new tab needs a workspace and
a size (from a fux pane, it takes that pane's tab's). `in` names the
workspace, made if it isn't there; `in` again nests one in it (`(in (in
(options) "proj") "agent1")`). `new-tab` answers its pane's version
(`%3:0.0.0.0.0:RUN`); here `rename` names the pane instead:

```sh
fux '(rename (new-tab (program (size (in (options) "agent") 80 24) "sh")) "sh1")'
```

**Drive it, and wait.** An action (`line`, `send-keys`, `signal`) answers
at once, before the program has read anything. A wait answers once the
pane changes: `shows` waits for a text (one only the output holds: the
line typed shows `$((6*7))`, never `42`), `quiet` for N seconds of no
change (a fact of the screen, not proof of an end), `until-ended` for
the program's exit; each leaves what it saw. Their seconds bound each
change, not the whole: `timeout 600 fux …` bounds a command (exit 124).
Keys: `(press PANE "C-c")` (`(key-names)` names them). To act only on the
screen you read, give its version: `expect` sends nothing and fails
`stale` if the pane changed since. A file's text goes as one paste.

```sh
fux '(line (pane "sh1") (first (arguments)))' 'sleep 1; echo "built: $((6*7))"'
fux '(shows (pane "sh1") "built: 42" 60)'
fux '(quiet (pane "sh1") 2)'
fux '(get (get (until-ended (new-tab (argument (argument (program (size (in (options) "jobs") 80 24) "sh") "-c") "exit 3")) 60) "program") "exited")'
V=$(fux '(version (capture (pane "sh1")))')
fux '(line (expect (first (arguments))) "echo yes")' "$V"
printf 'echo pasted\n' > lines.txt
fux '(send-keys (paste (pane "sh1") (input)))' < lines.txt
```

**Read it in one observation.** `(capture PANE)` gives the screen as text,
`(capture (history PANE 100))` with history, `(capture (last-output PANE))` the last
shell command's output (`(explain "shells")`). `get` on a capture reads the pane's
whole observation at one version: `lines`, `version`, `cursor`, `program`
(`process-id`, `state`, `exited`, `signal`, `foreground-group`), `last-status`.

**Follow what changes.** A capture's `cursor` is the journal's place at
that screen. `events` from it prints each later event of the pane (or of
`(all)`), one record a line with its `next`, until the scope is gone;
`head` (141 then) or `timeout` ends it, `>> FILE` keeps it. An entry lost before
it was read fails it (`journal-loss`): capture again. A screen's change is no event.

```sh
fux '(define! seen (capture (pane "sh1"))) (list (get seen "version") (get seen "program") (get seen "last-status"))'
C=$(fux '(get (capture (pane "sh1")) "cursor")')
fux '(line (pane "sh1") "echo after")'
fux '(events (pane "sh1") (first (arguments)))' "$C" | head -n 1 || [ $? -eq 141 ]
```

**Leave the OS's work to the OS.** fux has no operations for these:

| For | Use |
| --- | --- |
| a program's environment | `env(1)`: `(argument (argument (program OPTIONS "env") "PORT=8080") "./app")` |
| a time limit | `timeout(1)`, on fux or in a pane: `(argument (program OPTIONS "timeout") "60")` … |
| a file's text | `< FILE`, read by `(input)` |
| what a pane runs, and where | its `program`'s `foreground-group` G: `ps -o comm=,args= -p G`; Linux `readlink /proc/G/cwd`, `pgrep -a -g G`; macOS `lsof -a -p G -d cwd -Fn`, `pgrep -lf -g G` |
| a log of what a program wrote | `script(1)` in its pane (`(explain "servers")`) |

**Stop.** Close your workspace and all in it; kill your `events` readers
by PID. A server ends with its last pane (a crashed one leaves its panes
for the next `(start)`); one still up stops with `kill` on its process,
which macOS leaves out of `status`, where `lsof -t` names it:

```sh
fux '(kill (workspace "agent"))'
if fux '(status)' > /dev/null 2>&1; then kill "$(fux '(get (status) "server-process-id")' 2>/dev/null || lsof -t "$FUX_SOCKET")"; fi
```

fux exits 0 when done (or with what `(exit N)` gave); 1 when refused, saying `fux:
KIND: VALUES` on stderr; 2 for a mistake in the program (`usage`, `unbound-symbol`,
`type-error` …); 4 if the pane waited on closed; 124 on `timeout`; 141, quietly, when stdout's reader went.

More: `(explain "TOPIC")` for `recipes`, `typing`, `read`, `shells`,
`programs`, `servers` and `recipe-examples`; `(explain "guide")` all of them;
`(explain "cookbook")`.

## recipes: Waiting for what you need

Each is a grift program: a file `fux --file FILE` runs, or `fux -` reads
from stdin. A loop of waits, a guard on a version, a fallback: what must
happen inside one program. The shell's recipes are `(explain "cookbook")`'s.

`(wait A)` waits from the version answer A holds, so a change between two
steps is never missed, and leaves the next answer; `(wait (timeout A 600))`
fails with 124 (`timeout`) if nothing changed in 600 s; `(wait (changed V))`
waits from a version or a capture. A pane part way through a frame
(synchronized output) whose rest fux already holds keeps the answer until the
frame is read, then answers its change: a wait never says unchanged of a pane
that changed. To bound a whole loop of waits, run it under `timeout 600 fux
--file FILE` (124 too). A pane, an answer and a capture name their version's
counts, `respawns` `screen` `bells` `done` `ended`: `(get A "done")` is the
`DONE` of the version A holds.

A wait's text answer (`(string A)`) is its version, then what changed (`screen`,
`bell`, `done`, `respawned`), then `exited N`, `killed by SIGNAME` (or `killed by
signal N` for an unnamed signal), or `ended, its status lost` if the program
ended. A closed pane answers `%2 closed`. A tab or workspace wait freezes its
panes as it begins and answers changes in watch order, not exit chronology.

`shows` checks text presence; old text and input echo can match. Use a
reply-only result (42 is absent from 6*7), shell end marks, or a
TUI-specific predicate. A program that ends without the text fails
`ended-before-text`. `until-ended` checks ENDED before waiting, so an
already-ended program succeeds at once, and follows the pane if respawned.
A capture keeps screen, version and metadata together; `get` and `version`
on it read locally. `observed` retains a pane capture or samples a pane or
single answer. Exit status and signal are distinct; an unknown status
stays unknown.

- **Launch and wait for a prompt.** Run with `timeout 30 fux -` and this
  program on stdin; a banner alone is not the prompt:

  ```grift
  (define! p (new-tab (argument (program (size (in (options) "agent") 80 24) "python3") "-q")))
  (define! seen (shows p ">>>" 5))
  (print (string seen))
  (print (version seen))
  ```
- **Run a command, read its output and actual exit status.**
  Run with `timeout 30 fux -` and this program on stdin:

  ```grift
  (define! p (new-tab (argument (argument (program (size (in (options) "agent") 80 24) "sh") "-c")
    "echo result=42; exit 7")))
  (define! seen (until-ended p 10))
  (print (string seen))
  (print (get (get seen "program") "exited"))
  ```
- **Guard input against the screen you read.** Save this as `reply.grift`
  and run `timeout 30 fux --file reply.grift "$V"`, with the prompt's
  version above:

  ```grift not run here: agentbench runs it (published-prompt), answering its slow prompt
  (define! answer (send-keys (key (text (expect (first (arguments))) "6*7") "Enter")))
  (define! seen (shows answer "42" 5))
  (print (string seen))
  (print (version seen))
  ```
- **Quiet for N seconds** (an agent's TUI done, or asking you something):
  `quiet` waits until nothing changes for N seconds (from a pane, an
  answer, or pane capture). A spinner or clock on the screen is a
  change, so then wait for a text.

  ```grift not run: it needs an agent's TUI in pane 2
  (define! p (pane 2))
  (send-keys (key (paste p "fix the test") "Enter"))
  (string (quiet p 5))
  ```
- **A text the reply writes:** count it before, act, and wait until there
  is one more. Choose a text only the reply holds: here the question holds
  no `42`. If the input itself holds it, wait until the capture shows the
  input, then count, so it is in the count, not in what you wait for; a
  program that draws the input again (a TUI that keeps it above the
  reply) counts it again. `hits` counts the lines of the last 1000 of
  history and the screen, so a count can fall as lines scroll past them: choose a window the reply
  surely comes within.

  ```grift not run: it needs an agent's TUI in pane 2
  (define! hits (lambda! (p)
    (length (filter (lambda! (row) (not? (empty-list? (search row "42" 0))))
      (get (capture (history p 1000)) "lines")))))
  (define! until-more (lambda! (p seen n)
    (if! (greater-than? (hits p) n) "answered"
      (until-more p (wait (timeout (changed seen) 600)) n))))
  (define! p (pane 2))
  (define! n (hits p))
  (until-more p (send-keys (key (paste p "What is 6 times 7? Digits only.") "Enter")) n)
  ```
- **A shell command's end, with marks** (`shells` says how to turn them
  on): its `done` count grows as one ends. Type the line at a prompt:
  a command still running ends first, and counts. A line that runs nothing
  (an empty line, a comment, a syntax error) ends no command, so `done`
  stays: for such a line, wait for the prompt's text instead. (A comment
  line ends a command in fish, which marks it, and in zsh, which runs `#`
  as a command unless `setopt interactive_comments`; a syntax error is
  tested in bash and zsh alone.)

  ```grift not run: it needs a shell with marks in pane 3, and a make test
  (define! p (pane 3))
  (define! before (capture p))
  (line p "make test")
  (define! finished (grows before "done" 1 600))
  (print (get (pane finished) "last-status"))
  (capture (last-output finished))
  ```
- **A shell command's end, without marks:** print a text only its end can
  show, quoted so the line you type does not hold it, and wait for it:
  the line shows `END-OF""-RUN` as you type it, and `END-OF-RUN 0` at its
  end (`$status` in fish).

  ```grift not run: it needs a shell in pane 3, and a make test
  (define! p (pane 3))
  (line p "make test; echo END-OF\"\"-RUN $?")
  (shows p "END-OF-RUN" 600)
  ```
- **A program's exit:** `until-ended` checks first, so an already-ended
  observation succeeds immediately, including an unknown exit status. The
  exit is told as the program is reaped, even while something it left goes
  on writing; what its terminal holds is read on, and an ended pane is
  frozen once its terminal is drained.

  ```grift
  (define! p (new-tab (argument (program (size (in (options) "agent") 80 24) "cargo") "test")))
  (get (get (until-ended p 600) "program") "exited")
  ```
- **A bell or notification:** its `bells` count grows, as for a command's
  end: `(grows (capture p) "bells" 1 600)`.
- **A TUI's initial selection:** save as `selection.grift`; pass its pane
  as the first file argument. Here reverse video identifies the app's
  selection. That meaning belongs to this app, not to fux.

  ```grift not run here: agentbench runs it (published-tui) against its menu fixture
  (define! until-marked (lambda! (p needle)
    (define! seen (capture (marks (wrapped p))))
    (if! (empty-list? (search (string seen) needle 0))
      (sequence! (wait (timeout (changed seen) 5)) (until-marked p needle))
      seen)))
  (define! seen (until-marked (pane (first (arguments))) "⟦Alpha⟧"))
  (print (string seen))
  (print (version seen))
  ```
- **Change that TUI's selection:** save as `select-next.grift`, with the
  `until-marked` above first; pass the initial capture's version. This app
  uses Down to select Beta. Keep the marked view in the loop; `shows` would
  use plain screens on later reads.

  ```grift not run here: agentbench runs it (published-tui) against its menu fixture
  (define! answer (press (expect (first (arguments))) "Down"))
  (define! seen (until-marked (pane answer) "⟦Beta⟧"))
  (print (string seen))
  (print (version seen))
  ```
- **What changes anywhere:** the page's `events`, and the cookbook's
  "Follow events from a cursor" and "Keep a journal file". A hook is an
  ordinary program over that stream, filtering the triggering event and
  state as well as its origin (an outside command line is `outside`); no
  exactly-once effects are promised. `(next-events (all) CURSOR SECONDS)`
  returns one batch instead: its `events`, the oldest entry still held
  (`first`), exact `next`, `missed` and `status`.
- **First recorded exit across jobs:** save this as `first-exit.grift` and
  run `timeout 30 fux --file first-exit.grift` on the private socket. Take the
  cursor before launching, even if jobs might finish before subscription.
  Replay order is server-recorded order, not physical OS termination order.
  Loss is a failure (`batch-events` raises it); resnapshotting cannot recover which
  missing exit was first. Any of the three may be recorded first: launch
  latency can change their order. The result includes the cursor, the panes and
  the first exit record as an auditable result. Reads are finite batches, and
  every launched pane is cleaned up. No group wait is used.

  ```grift
  (define! cursor (get (information (all)) "cursor"))
  (define! job (lambda! (seconds)
    (pane (new-tab (argument (argument (program (size (in (options) "agent") 80 24) "sh") "-c")
      (join "sleep " seconds))))))
  (define! jobs (map job (list "0.3" "0.05" "0.15")))
  (define! ours? (lambda! (event)
    (and!? (same? (get event "event") "exited")
      (member? (string (get event "pane")) (map string jobs)))))
  (define! first-exit (lambda! (from limit)
    (when! (same? limit 0) (raise (error (quote! fail) "event batch limit reached before an exit")))
    (let! ((batch (next-events (all) from 5)) (exits (filter ours? (batch-events batch))))
      (conditional!
        ((not? (empty-list? exits)) (first exits))
        ((member? (get batch "status") (list "gone" "stopped"))
          (raise (error (quote! fail) "event read ended before an exit" (get batch "status"))))
        (#true (first-exit (get batch "next") (subtract limit 1)))))))
  (finally! (display (list cursor (map string jobs) (first-exit cursor 256)) "\n")
    (for-each kill jobs))
  ```
- **A deadline, a fallback, a retry:** `catch-kind!` catches the failures it
  names (`programs`). A key, and C-c if nothing came of it in 10 s:

  ```grift not run: it needs a program in pane 2 that asks for a key
  (define! p (pane 2))
  (define! a (type p "y"))
  (catch-kind! (wait (timeout a 10)) timeout failure (press p "C-c"))
  ```
  A command's output alone where the shell marks commands, else the screen:
  `(catch-kind! (capture (last-output p)) no-marks failure (capture p))`.
- **An agent's whole answer:** on the alternate screen a TUI redraws its
  whole screen each frame and nothing scrolls off, so fux keeps no
  history of it. Run it inline (`codex --no-alt-screen`, `claude
  --settings '{"tui":"default"}'`) and `fux '(capture (all-history (pane 2)))'` reads
  all of it.
- **Whether a program waits for keys:** fux does not say; no system
  reports it as a fact. Wait for what the program shows when it asks (a
  prompt's text), or for quiet.

## typing: Keys, the mouse and stopping

- **Keys** go between the pane and `send-keys`, each wrapping the one before:
  `(text T "x")`, `(key T "NAME")` (tmux's names: `Enter`, `Escape`, `Tab`, `BSpace`,
  `Up`, `PageDown`, `F5`, `C-c`, `M-s`, `S-Tab`; `Esc`, `Backspace`, `PgUp` too; a typo
  is refused, `not-a-key`; `(key-names)` lists the vocabulary), `(paste T "x")` (bracketed if
  the program asked), `(bytes T #bytes(27 91 65))`, of any length. `(type P "x")` and
  `(press P "Enter")` send one; `(line P "x")` a text and Enter. A key N times is a fold:
  `(send-keys (fold (lambda! (keys #ignore) (key keys "Down")) (pane 2) (range 0 3)))`.
  A file's text, whole, from stdin (`(input)`):

  ```sh not run: FILE is your file
  fux '(send-keys (paste (pane 2) (input)))' < FILE
  ```
- They are written together, in order, as the program's terminal takes
  them; an action answers once they are queued, not once the program
  read them. A program that tells a lone Escape from a key's sequence by
  time (Vim, readline) may read Escape and a key sent soon after it, in one
  `send-keys` or two, as one key: send Escape, wait until the program shows
  it took it (its mode line), then the next key. Keys sent to a program
  that exited or that closed its terminal (where the system says so:
  Linux) are refused (`not-running`). Keys a program does not read wait
  for it, as many as memory holds.

  ```grift not run: it needs Vim in insert mode in pane 2
  (define! p (pane 2))
  (define! until-gone (lambda! (seen)
    (if! (empty-list? (search (string (capture p)) "INSERT" 0)) seen
      (until-gone (wait (timeout (changed seen) 5))))))
  (until-gone (press p "Escape"))
  (line p ":wq")
  ```
- **Keys at the screen you saw:** `(expect SEEN)` makes keys for SEEN's
  pane that `send-keys` sends only if the pane is still at version SEEN
  as they are queued; else it sends none and fails `stale`. SEEN is a
  pane's capture (its version as fux read the screen, as its `version`
  field says), an action's or wait's answer, or a version as text.
  Every count of the version (above) is a change. That is all it
  promises: the pane was where you saw it when the keys were queued, not
  that its program has not moved on before it reads them, nor what output
  is on its way that fux has not read. A pane closed since fails
  `no-pane` instead (a shell pane closes as its shell exits; a pane
  started with a program stays, and fails `stale`); a version of another server
  run, or with counts the pane can't have had, fails `bad-value`, as for
  `changed`. A version is one pane's: to a
  tab's panes, send each its own; keys to a client go to the pane the
  client chooses, and take no version. Keys to a tab or a workspace go
  to each of its panes on its own, each checking its own: one that
  refuses fails the whole, saying which panes took them (`not-running: …;
  already sent to %2 %4`). A pane whose own process ended before it said
  whether it took them fails `indeterminate`: it may have.

  ```grift not run: it needs a program in pane 2 that asks for y
  (define! seen (capture (pane 2)))
  (line (expect seen) "y")   ; stale if %2 changed since
  ```
- **Mouse:** `(send-mouse (pane 2) "click" ROW COLUMN)` (also `"right"`,
  `"middle"`, `"press"`, `"release"`, `"wheel-up"`, `"wheel-down"`, `"move"`), cells
  from 0 as `(capture (wrapped p))` counts them; on a double-width row, click at
  2×COLUMN. A program's own "row 22" is usually counted from 1. A program that
  asked for no mouse reports refuses it (`mouse`).
- **Stop:** `(signal (pane 2) "TERM")` (`"INT"`, `"KILL"`, …) signals what runs in
  its terminal's foreground; `(kill (pane 2))` closes the pane; `(respawn (pane 2))` starts
  the pane's program again once it ended (`(respawn (kill-first (pane 2)))` ends it first; a
  shell's pane is refused, `not-run`). The new program starts at once, and
  takes over once what the old one left is read, in turns as any output is,
  and its exit told. Meanwhile the old program is let go and the pane runs
  none of its own: input is refused (`not-running`), as are `signal` and
  another respawn (`still-running`); closing the pane ends the new program,
  and the respawn answers `closed`. The respawn goes on if its caller goes.

## read: Reading the screen

For metadata, take one `snapshot`: `(snapshot (workspace "agent") "topology")`
holds the workspace's ordered tabs, panes, tree and client placement. Its
`(get S "panes")` is a local list of records; each record's `(get R "id")` is a
handle and `(get R "title")` the retained title. Topology makes no process
metadata queries; `"facts"` gives each pane's whole observation, its program,
launch and terminal state with it, as a capture does, and `"recipe"` adds
where each program is now. A record and its child records retain the facts
of that observation: reading a field of a held record does not refresh it,
and reading a field of a handle asks the server. `(datum VALUE)` exposes a
held answer or capture as records and lists, and `(data-text VALUE)` is a
string of readable grift data.

`(capture (READ PANE))`, READ any of: `(history PANE 100)`, `(all-history PANE)`,
`(main-screen PANE)` (what was printed before a program took the screen),
`(alternate-screen PANE)` (the full screen a program drew last), `(last-output PANE)` (the
last shell command's output, or the running one's so far, between its
marks: what a clear erased there reads blank, and an ended command's
last row ends at its end mark, so a last line ending in `\r` reads
empty), `(wrapped PANE)` (each screen row a line; else a line wrapped at the
pane's width is joined whole), `(rows PANE 0 4)`, `(columns PANE 0 40)`, `(full PANE)`, `(marks PANE)`
(reverse video and any background colour as `⟦this⟧`: a program that
paints its whole background is marked whole; text rendered faint as
`⟨this⟩`), `(styles PANE)`, `(since PANE VERSION)` (only the lines with a row
changed since that version, each written after its row, `3:text`;
`stale` where fux can't say which: after a scroll, resize or respawn).
They wrap one another: `(capture (rows (wrapped p) 0 4))`.
Blanks at a line's end are trimmed, so a prompt's `>>> ` reads `>>>`;
`full` keeps every cell. `get` then reads the screen and the pane's whole
observation, its fields as `information` gives them read in the same turn:
`screen-cursor` is `(ROW COLUMN)` (`rows` and `columns` the
screen's size), `alternate-screen` whether a program holds the
alternate screen, `history-lines` the history kept, `above` the history
rows kept above the first row read, `wrapped-rows` which rows go on in
the next, `links` the hyperlinks (OSC 8) as `row`, `column`, `length` and `uri`,
`last-output` where `last-output` reads, `cursor` the journal's place
read with it (`events` from it). `docs/records.md` says every field. Rows and columns are half-open
`start..end` ranges from 0; equal endpoints select nothing.
A joined row range includes each whole logical line it touches; `wrapped`
selects physical rows only.
Every pane of a tab, a workspace (with the workspaces in it) or `(all)`, in
layout order: `(map capture (panes (workspace "agent")))`; `tabs`
gives its tabs the same way.

## shells: Shells and their marks

`fux '(new-tab (size (in (options) "agent") 80 24))'` gives a shell (run in a fux
pane, the size may go: the tab is then laid out on that pane's tab). fux starts shells as they
are; a shell that sends OSC 133 marks (`C` as a command's output begins,
`D;STATUS` as it ends, `A` at a prompt) counts the commands that end in
its pane's version (`recipes`), gives `last-status`, lets `last-output`
read a command's output alone. fux reads marks from any program that sends them. Without marks
the count stays, and `last-output` refuses with `no-marks`. A `D` with no `C`
before it ends nothing: a line that runs nothing (an empty line; a
syntax error in bash and zsh; a comment in bash) runs no command, so it
sends no `C`, and `DONE` and `last-status` stay. fish marks a comment line
as a command, and zsh runs `#` as one unless `setopt
interactive_comments`. Readiness remains the caller's decision. To send them, in your own startup files:

```bash run by tests/agents.rs
# bash 4.4 or later (macOS's /bin/bash 3.2 has no PS0), ~/.bashrc
PS0='\e]133;C\a'
mark_end() { local s=$?; printf '\e]133;D;%s\a\e]133;A\a' "$s"; return "$s"; }
PROMPT_COMMAND=mark_end${PROMPT_COMMAND:+;$PROMPT_COMMAND}  # first: the hooks after it see $?
```

```zsh run by tests/agents.rs
# zsh, ~/.zshrc
autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
precmd_functions=(mark_end $precmd_functions)  # first, so $? is the command's
```

```fish run by tests/agents.rs
# fish, ~/.config/fish/config.fish
function mark_start --on-event fish_preexec; printf '\e]133;C\a'; end
function mark_end --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
function mark_prompt --on-event fish_prompt; printf '\e]133;A\a'; end
```

In bash, a hook after `mark_end` sees the command's `$?`, though
`PIPESTATUS` then holds `$?` alone. A remote shell (`ssh`) marks commands if
its startup does. Or run a remote command in a pane of its own, and wait
for its exit: `(new-tab (argument (argument (argument (program (size (in (options) "agent") 80 24)
"ssh") "-n") "HOST") "make"))`.

## programs: A program

`(require! "profiles/agent.grift")` reads a file's forms where it stands, once
per file, relative to the requiring file (or the cwd, for the command
line and stdin); `(include! "FILE")` reads one each time, in the caller's context. A source is any file that
reads, a FIFO or `<(…)` among them; a cycle fails. No config is loaded
by a command line unless it requires it.

A file of grift runs with `fux --file FILE ARGS…` (or `fux - ARGS…` from stdin;
a script may start `#!/usr/bin/env -S fux --file`), and keeps its names
between steps:

```grift
; test.grift: run the tests, wait until they end, read what they printed
(define! a (new-tab (argument (program (name (size (in (options) "agent") 80 24) "tests") "cargo") "test")))
(until-ended a 600)
(capture (all-history a))
```

`name` there names the tab (`(tab "tests")`). A pane is named by a second
request on its answer or handle: `(rename ANSWER "runner")`, then
`(pane "runner")`. A name is one thing's: a workspace, tab or pane that
has it already refuses it to another (`name-taken`).

`(define! NAME VALUE)` names a value, `(lambda! (X) ...)` makes a function,
`if!` (both branches, and a boolean test), `when!` and `conditional!` decide,
recursion repeats, `(list ...)` gathers
values, `map`, `filter`, `fold` and `for-each` run over a list, and `(catch-kind!
EXPRESSION KIND NAME HANDLER...)` runs its handler instead of failing with a
failure of KIND: `(catch-kind! (wait (timeout a 5)) timeout failure ...)`.
`(catch! EXPRESSION NAME HANDLER...)` takes any. The handler sees one failure,
whose fields `get` reads: `kind`, `values` (a message, then what it carries,
such as a failed wait's answer) and `trace`, the forms it came through;
`(raise failure)` raises it again, and `(finally! EXPRESSION CLEANUP...)`
always cleans up: after a failure, and after a stop (the prefix, then Esc),
when the cleanup's requests still reach the server and what they did is
listed. A cleanup's own failure after a stop doesn't show: the notice says
`stopped` and what took effect. A second stop ends every cleanup left,
cleanups inside cleanups too, and a process killed outright, as a signal ends a command line,
cleans up nothing. A failure it does not name goes on, its status kept. An
action leaves its answer: printed, it is the lines above; given to an
operation that wants a pane, it is its pane (an action's or wait's on a tab
or workspace is a list of answers, which no such operation takes, however
many panes it has: name the tab again, `(wait (tab 1))`), and to `wait`, the
version it holds too (`(wait (type p "y"))` waits from before the keys; a
name keeps that version, so `(wait t)` later waits from when `t` was made, and
`(wait (changed (get t "version")))` from the same held version).

Modifiers retain whether a request names a pane, tab, workspace or client.
`changed` makes a request `wait` alone takes; `expect` makes one `send-keys`
alone takes. A pane capture has kind `capture` and a version; a rendered
client screen has kind `client-capture` and none. A caught closed pane has
kind `closed-answer`: it can name its pane (which is gone), but supplies no
version to `changed` or `expect`. `(kind VALUE)` says which a value is. A
handler may also receive an ordinary message string, so uncertain values
still need runtime checks.

`string` converts a completed value to text (`(string (capture p))`). A
request with unused options must first reach its action: a program that ends
holding one fails `unused`, and converting it with `string` does not complete
it. An unbound name fails `unbound-symbol`; fux does not infer an executable
name or a correction. `fux '(help)'` lists fux's library, written in grift, and
`built-in-names` grift's own.
The whole program is read before anything runs, so a form that doesn't read
(`parse-error`) acts on nothing. After that forms run in order, so the forms before
a failing one have already acted: stderr lists each step that took effect, once,
after why it failed (`fux: already took effect: (rename ...)`), and `(actions)` is the
same list inside a program. A run exits 0, the last value
printed; or the one `(exit N)` gives (0 to 255); or with the status of the
failure that stopped it (above), stdout getting nothing more and stderr
why.
A request acts only when a word sends it or it is the program's last value, so
a request left unused does nothing and says nothing: `(key (pane 1) "Enter")
(print "sent")` sends no key and exits 0. Write `(send-keys (key (pane 1)
"Enter"))`.
fux itself never runs text from a screen (D-151), but a program can, with
`read-form` and `evaluate`. A pane shows what anything wrote there, so don't
evaluate text you captured from one.
`(environment-variable "NAME")` reads a local variable in config, a binding or a
command line; an unset variable fails `unset-variable`, never an empty string.
From the command line alone, a script reads `(arguments)` (the strings after
the program, or after `--file FILE`), `(input)`
(the rest of stdin; not when the program came from it), and `(read-file
"PATH")`. `(warn VALUE)` prints on stderr:
`(when! (empty-list? (arguments)) (warn "usage: job.grift PANE") (exit 2))`.

An attached typing client loads `$XDG_CONFIG_HOME/fux/config.grift`, or
`$HOME/.config/fux/config.grift`. The complete candidate config is checked before it becomes
live. A failure keeps the bundled keys and reports its file, line and value.

```grift config
(set "history-bytes" 16777216)
(bind-key "C-b y" (code! (lambda! ()
  (split (axis (focused (me)) "horizontal")) #inert)))
(label "split right")
(group "Panes")
```

A client's later jobs use the sources frozen when its config loaded. Callback
forms use `code!`, with immutable captured arguments supplied explicitly as
`(list captured-arguments code-value)`. At the command prompt (`C-b e`), type a
grift program. The prefix followed by Escape stops an active program, including a
blocked wait. Rust keeps drawing while that worker runs.

## servers: Servers, sizes and more

A saved session recreates programs, directories, names and layouts with new server identities:

```sh
fux '(dump (all))' > "$TMPDIR/session.grift"
```

Run it on a newly started server with `fux --file "$TMPDIR/session.grift"`.
`kill` closes a target; `kill-server` refuses an attached person or an invocation from one
of that server's panes. Each script should clean up the targets and processes it started.

- A pane's program takes the server's environment, plus `FUX`,
  `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER`. `env(1)` sets the rest:
  `fux '(new-tab (argument (argument (program (size (in (options) "agent") 80 24) "env") "PORT=8080") "./app"))'`.
- Handle values returned by a server retain its run through lists, bindings,
  answers and request options; using one against another run is refused.
  A hand-written `(pane 3)`, `(tab 2)`, `(workspace 1)` or `(client "c1")` means the server
  `FUX_SOCKET` names, now. Its text (`print`, `string`) is the bare id, and loses the run: that text is not durable
  across restarts.
- `FUX_SOCKET=PATH fux …` talks to the server on that socket (an absolute
  path), else `server.sock` in fux's directory; `(start)` starts one, with
  nothing in it (the first client to attach starts a shell on its screen;
  one that only watches is refused, as there is nothing to watch), `(status)`
  says which answers, and fails (`no-server`) where none does. One `fux` command
  talks to one server; join two with a
  pipe: `fux '(capture (pane 1))' | FUX_SOCKET="$XDG_RUNTIME_DIR/fux/b.sock" fux
  '(send-keys (paste (pane 1) (input)))'`. The servers in fux's directory (`$XDG_RUNTIME_DIR/fux` whenever
  `XDG_RUNTIME_DIR` is set, else `$TMPDIR/fux`; on Windows `%LOCALAPPDATA%\fux`) are its
  `.sock` files, on Windows the `.server` records beside the pipes they name: `status` each.

  ```sh
  for s in "${XDG_RUNTIME_DIR-$TMPDIR}"/fux/*.sock; do [ -S "$s" ] || continue; FUX_SOCKET=$s fux '(status)'; done
  ```
  Inside a person's pane, `(here)` is that pane. Every pane has
  `FUX_PANE` (`%N:GENERATION`, the pane and its program generation),
  `FUX_SOCKET` (its server's socket), `FUX_SERVER` (which run of the server:
  a `FUX_PANE` of another, or with no `FUX_SERVER`, is no `here`:
  `stray-pane`; neither is a caller from before a respawn) and `FUX`
  (the fux the server runs, for a script that may not find `fux` on PATH;
  unset if the server cannot name it). Resolve `FUX_PANE` with `(here)`;
  its text before the colon alone is the pane id. A person
  who wants your panes apart from theirs gives you a server of your own
  (`FUX_SOCKET=PATH fux '(start)'`), which they watch by attaching to it.
  `(kill-server)` from inside its own server is refused;
  `kill` on its process stops it: `kill "$(fux '(get (status) "server-process-id")')"`,
  or on macOS, which says no `server-process-id`, the pid `lsof -t SOCKET` gives.
  A command line against a server of another
  fux protocol runs again with that server's own binary, saying so in one
  line on stderr; where that cannot be, it fails with `mismatch`, saying why.
  A command whose reply was lost has an unknown outcome: the server may
  have made a process or queued its input, so running it again is no way
  to learn whether it ran.
- Nothing is saved on its own: a server that stops takes its session
  with it. One that crashes leaves its panes' processes waiting, each with
  its program, its screen and its history, beside its socket; `(start)`
  on that socket takes the run back: its ids, names, layout and versions
  go on, and `FUX_PANE` still names each pane. A pane whose own process
  ended while no server ran stays as its role says: a program's ended, its
  screen blank, a shell's closed. Its journal is new. Where
  the run cannot be taken back (its record or its fux gone), `start`
  refuses, naming the panes that wait and their pids. `fux '(dump (all))' > FILE` saves its layout and launches when you
  choose; `fux '(start)' && fux --file FILE` makes them again on a fresh server,
  never its processes' state. That is another server run: versions from
  before it are refused.
- fux is controlled through its Unix socket (on Windows, its named pipe) alone: a sandbox that denies
  `connect` to it (Codex's denies every `connect`) cannot drive fux. Run
  fux commands outside such a sandbox, or allow the socket.
- `(attach (tab 2))` shows that tab's first pane (its zoomed one, if zoomed); `(attach (pane 3))` focuses that pane.
  `(attach (in (options) "work"))` requires that workspace to exist. Without a target,
  attach uses the first stored workspace; a writable attach carries its launch,
  which only an empty server starts, serialized by the server. One viewport
  supplies the pane size (less a bar row, if its bar has one). Watchers never create.
  `(attach-defaults (in (options) "work"))` is a local preference; explicit attach
  operands win. A fresh typing client loads config edits; existing clients
  keep their frozen configuration.
- A tab is laid out once, on its effective size: one given a `size`
  (`(resize TAB 40 12)`) always on that; else (`(resize-auto TAB)`) on the smallest
  screen of the clients showing it, watchers left out, while every pane fits there, else on the last size that did.
  `(resize-pane PANE (percent 95) "horizontal")` sizes a pane in its tab; its `row` and `column` say where it is. A
  size that cannot be had is refused (`size`, as is a screen past what a
  grid holds, from `(size OPTIONS COLUMNS ROWS)` or the server), and nothing changes; so is a split
  or a move that would hide a pane on that size. A larger screen shows the
  tab at the top left, the rest blank.
- To keep what a program writes, run it under `script` in its pane, so it
  still writes to a terminal (a pipe, as with `tee`, changes what it
  writes). With util-linux:

  ```grift
  (new-tab (fold argument (program (size (in (options) "agent") 80 24) "script")
    (list "-q" "-e" "-f" "LOG" "-c" "make test")))
  ```
  on macOS and the BSDs, `(list "-q" "-F" "LOG" "sh" "-c" "make test")`. LOG holds the
  terminal's raw bytes (util-linux adds a first and last line of its own).
  `(dump (all))` prints the layout as a program that makes it again;
  `(attach (size (options) 100 30))` is a client with no terminal (`(send-keys (key
  (client "c2") "Enter"))` types at it, `(capture (client "c2"))` shows it): how to drive
  fux's own screens. Each client is shown its own workspace, tab and pane:
  `(show (client "c2") (pane 3))` shows it `%3`, and `(step (client "c2") (tab 2) "next")`
  the tab after `@2` (`"last"`: the one it was shown there before; a pane also steps
  `"left"` and so on).

A split needs an explicit axis: `(split (axis (pane 2) "horizontal"))`; resize
with `(resize-pane (pane 2) (percent 50) "horizontal")`. `panes` and `tabs`
read ordered membership; `(information (all))` reads one facts snapshot.

## recipe-examples: Programs over a pane argument

Save one block to a file and run `fux --file example.grift "$pane"`.
The pane is a shell argument string, never source pasted into the program.

### Quiet after a request

```grift run by tests/agents.rs
(define! p (pane (first (arguments))))
(line p "go")
(string (quiet p 1))
```

### Wait for a reply

```grift run by tests/agents.rs
(define! p (pane (first (arguments))))
(line p "question")
(shows p "the answer is 42" 5)
(print "answered")
```

### Read a marked command

```grift run by tests/agents.rs
(define! p (pane (first (arguments))))
(define! before (capture p))
(line p "go")
(define! ended (grows before "done" 1 5))
(define! output (capture (last-output ended)))
(print (get output "last-status"))
(string output)
```

### Read an unmarked command

```grift run by tests/agents.rs
(define! p (pane (first (arguments))))
(line p "true; printf '\\nEND-OF-RUN %s\\n' \"$?\"")
(string (shows p "\nEND-OF-RUN 0\n" 5))
```

### Read a program exit

```grift run by tests/agents.rs
(define! p (pane (first (arguments))))
(get (get (until-ended p 5) "program") "exited")
```

### Fall back after a deadline

```grift run by tests/agents.rs
(define! p (pane (first (arguments))))
(catch-kind! (shows p "never-arrives" 1) timeout failure
  (press p "C-c")
  #inert)
```
