# The fux cookbook

Short recipes for what agents and people do with fux. Each is a heading,
what it's for, and a block to copy. The `sh` blocks run as they stand, top
to bottom, each in a shell of its own with `set -e -o pipefail`, against
the server `FUX_SOCKET` names: the second recipe makes the shell named
`shell` that later ones type into, and the last closes everything they made. A command expected to fail says
the exit status it expects, `|| [ $? -eq N ]`. A test runs every recipe,
so a recipe here is one this fux runs.

**The boundary:** the shell composes `fux` commands, grift does what must
happen inside one request or one key binding, and ordinary programs do
everything else.

fux has no files to read or write; the `fux` command is the same idea
spelled as commands, and a file is your own redirect:

| As a file, it would be | With fux |
| --- | --- |
| `cat screen` | `fux '(capture (pane 2))'` |
| `echo X > input` | `fux '(line (pane 2) "X")'` |
| `tail -f events` | `fux '(events (all) "")'` |
| `cat events >> FILE` | `fux '(events (all) "")' >> FILE` |

## Pick a server

`FUX_SOCKET` names the server every `fux` command talks to. `(start)`
makes the socket's directory, only you allowed in (mode 0700), and
refuses one anyone else may open; it does nothing where a server already
answers. A server of your own keeps your panes
apart from anyone else's; stop it with `kill` on its process, which macOS
leaves out of `status`: there `lsof -t` names it from the socket.

```sh
(
  export FUX_SOCKET="$PWD/mine/fux.sock"
  fux '(start)'
  kill "$(fux '(get (status) "server-process-id")' 2>/dev/null || lsof -t "$FUX_SOCKET")"
)
```

## A shell to type into

A new tab in workspace `work` running `sh`, its pane named `shell`: the
later recipes type POSIX `sh` into it, whatever your `$SHELL` is (leave
out `(program ... "sh")` for that one). From outside fux a new tab needs a
size; run inside a fux pane, it takes that pane's tab's.

```sh
fux '(rename (new-tab (program (size (in (options) "work") 80 24) "sh")) "shell")'
```

## Run a program and wait for it to end

Start it in a new tab, or in a split of a pane, and print its exit status.

```sh
fux '(get (get (until-ended (new-tab (argument (argument (program (size (in (options) "work") 80 24) "sh") "-c") "sleep 1; exit 3")) 60) "program") "exited")'
fux '(get (get (until-ended (split (argument (argument (program (axis (pane "shell") "vertical") "sh") "-c") "exit 5")) 60) "program") "exited")'
```

## Type a line, send keys

`line` types a line and presses Enter; `send-keys` sends text and named
keys (`fux '(key-names)'` lists the names).

```sh
fux '(line (pane "shell") "echo hello")'
fux '(press (pane "shell") "C-c")'
fux '(send-keys (key (text (pane "shell") "echo hello again") "Enter"))'
```

## Shell arguments remain strings

Anything after the quoted program stays a string, whatever it holds, so text
from the shell is never source. `(arguments)` reads it.

```sh
fux '(line (pane "shell") (first (arguments)))' 'echo "parentheses (stay data)"'
fux '(shows (pane "shell") "parentheses (stay data)" 10)'
```

## Wait for text on the screen

Wait for a text only the output holds: the typed line shows `$((6*7))`,
never `42`. `shows` prints the screen it matched. Its 30 seconds bound
each wait for a change, not the whole: a screen that changes without the
text keeps it waiting. For a deadline on the whole, use `timeout(1)`
(below).

```sh
fux '(line (pane "shell") (first (arguments)))' 'sleep 1; echo "built: $((6*7))"'
fux '(shows (pane "shell") "built: 42" 30)'
```

## Wait for a quiet screen

`quiet` waits until nothing on the screen has changed for that many
seconds, then prints it. Quiet is a fact about the screen, not proof the
program is done.

```sh
fux '(quiet (pane "shell") 2)'
```

## Paste a file

The file's text goes to the program whole, as one paste (bracketed if the
program asked for it), however long it is. A paste into an attached
terminal arrives whole too.

```sh
printf 'echo pasted\n' > lines.txt
fux '(send-keys (paste (pane "shell") (input)))' < lines.txt
```

## Read the screen and the history

As text: the screen, the last 100 lines of history and the screen, or all
of it.

```sh
fux '(capture (pane "shell"))'
fux '(capture (history (pane "shell") 100))'
fux '(capture (all-history (pane "shell")))'
```

As data, read inside one program, so every field comes from one observation
(`docs/records.md` says every field). `(datum CAPTURE)` is the whole record,
and `(data-text VALUE)` the text of it:

```sh
fux '(define! seen (capture (pane "shell"))) (list (get seen "version") (last (get seen "lines")))'
fux '(get (datum (capture (pane "shell"))) "lines")'
fux '(define! held (information (all))) (map (lambda! (p) (list (string (get p "id")) (get p "name") (get (get p "program") "exited"))) (get held "panes"))'
```

A program in another language reads printed data with its own parser; a
grift program reads it with `read-form`, which evaluates nothing:

```sh
fux '(data-text (status))' > status.data
fux '(get (first (read-form (input) 0)) "protocol")' < status.data
```

## What a pane runs, and where

fux gives the pane's program's process (`process-id`) and its terminal's
foreground process group (`foreground-group`), in its `program` record;
the OS says what they run. The
group's leader may have exited while the rest of a pipeline runs on:
then `ps -p` fails, the leader's command and directory are unknown, and
`pgrep -g` still lists the group. Here a second pane, `piped`, runs a
pipeline whose leader `true` exits at once.

```sh
fux '(rename (new-tab (program (size (in (options) "work") 80 24) "sh")) "piped")' > /dev/null
fux '(shows (pane "piped") "$" 30)' > /dev/null
fux '(line (pane "piped") "true | sleep 300")' > /dev/null
until G=$(fux '(get (get (pane "piped") "program") "foreground-group")') &&
  [ "$G" != "$(fux '(get (get (pane "piped") "program") "process-id")')" ] && ! kill -0 "$G" 2> /dev/null; do sleep 0.1; done
```

```sh
for p in shell piped; do
  G=$(fux '(get (get (pane (first (arguments))) "program") "foreground-group")' "$p")
  if ps -o comm=,args= -p "$G"; then
    if [ "$(uname)" = Linux ]; then
      tr '\0' '\n' < "/proc/$G/cmdline"
      readlink "/proc/$G/cwd"
    else
      lsof -a -p "$G" -d cwd -Fn
    fi
  else
    echo "$p: group $G's leader is gone"
  fi
  if [ "$(uname)" = Linux ]; then pgrep -a -g "$G"; else pgrep -lf -g "$G"; fi
done
fux '(kill (pane "piped"))'
```

## Follow events from a cursor

`events` prints the journal's events, one record a line, each with
its `next` cursor, until the scope is gone or the server stops. Take a
cursor before you act and you miss nothing in between; `head`, `grep -m 1`
or `timeout` ends it. When its reader stops early, fux ends quietly with
141, as `cat` does.

```sh
C=$(fux '(get (information (all)) "cursor")')
fux '(new-tab (program (size (in (options) "work") 80 24) "true"))' > /dev/null
fux '(events (all) (first (arguments)))' "$C" | grep -m 1 '(event "exited")' || [ $? -eq 141 ]
```

An empty cursor starts now; to go on after a stop, pass the last line's
`next`.

## Read the screen, then follow what changes

A capture holds `cursor`, the journal's cursor read with its screen:
`events` from it starts reading the pane's journal entries after the
capture, none from before (as from any cursor, entries evicted first are
`missed`). A change only to the screen is no entry: it shows as a new
version and in captures. Here the first event after the capture is the
line typed after it.

```sh
C=$(fux '(get (capture (pane "shell")) "cursor")')
fux '(line (pane "shell") "echo after")'
E=$(fux '(events (pane "shell") (first (arguments)))' "$C" | head -n 1) || [ $? -eq 141 ]
[ "$(fux '(get (first (read-form (first (arguments)) 0)) "event")' "$E")" = input ]
```

## Watch what changes on a screen

`since` a version reads only the lines with a row changed after it, each
after its row as `grep -n` writes lines; its record holds the rows as
`changed-rows`, and the `version` to read from next. It suits a program
that redraws in place. Where fux can't say which rows changed (they moved
in a scroll or resize, or the program respawned) it fails `stale` (exit
1): capture the pane whole and go on from its version. Here the program
redraws its first row once it reads a line.

```sh
fux '(rename (new-tab (argument (argument (program (size (in (options) "work") 40 6) "sh") "-c") "stty -echo; printf \"a\\nb\\nc\"; read x; printf \"\\033[1;1HA\"; exec sleep 60")) "watched")'
fux '(shows (pane "watched") "c" 30)' > /dev/null
V=$(fux '(version (capture (pane "watched")))')
fux '(line (pane "watched") "x")'
fux '(shows (pane "watched") "A" 30)' > /dev/null
[ "$(fux '(capture (since (pane "watched") (first (arguments))))' "$V")" = 0:A ]
fux '(define! seen (capture (since (pane "watched") (first (arguments))))) (list (get seen "changed-rows") (get seen "version"))' "$V"
fux '(catch-kind! (capture (since (pane "watched") (first (arguments)))) stale failure (capture (pane "watched")))' "$V"
fux '(kill (pane "watched"))'
```

## Keep a journal file

The journal file is your redirect, written as long as the reader runs;
read it with `grep` or `tail -f`. Here the loop waits until the
file holds the program's exit, failing if the reader ended (`kill -0`),
then stops the reader.

```sh
C=$(fux '(get (information (all)) "cursor")')
fux '(events (all) (first (arguments)))' "$C" >> journal.data &
J=$!
fux '(new-tab (program (size (in (options) "work") 80 24) "true"))' > /dev/null
until grep -q '(event "exited")' journal.data; do kill -0 "$J"; sleep 1; done
kill "$J"
```

## Act only on the screen you saw

`expect` sends keys only if the pane is still at the version you read;
otherwise it sends nothing and fails `stale` (exit 1): read the screen
again.

```sh
V=$(fux '(version (capture (pane "shell")))')
fux '(line (expect (first (arguments))) "echo yes")' "$V" || [ $? -eq 1 ]
```

## Handle a refusal by kind

Inside a program, `catch-kind!` runs its handler for a failure of that kind
and lets every other go on; `get` reads the failure's `kind`, `values` and
`trace`.

```sh
fux '(catch-kind! (kill (pane 999)) no-pane failure (first (get failure "values")))'
```

## Stop a runaway with timeout

fux sets no time limits of its own: `timeout(1)` bounds a fux command
(exit 124 when it stops it), and a program in a pane.

```sh
timeout 2 fux '(shows (pane "shell") "never shown" 600)' || [ $? -eq 124 ]
fux '(get (get (until-ended (new-tab (argument (argument (argument (program (size (in (options) "work") 80 24) "timeout") "2") "sleep") "600")) 60) "program") "exited")'
```

## Cap one pane's memory

Each pane runs in a process of its own (`fux __pane`), its program's
parent, which holds its terminal: its screen, its history and the input
waiting for its program. fux sets no limit of its own; the system's caps
that process, and one past its cap ends alone. Its pane reads
`read-failed`, saying how its process ended, then its program's end; the
server and every other pane go on. `prlimit` (Linux) caps one pane's
address space:

```sh
P=$(ps -o ppid= -p "$(fux '(get (get (pane "shell") "program") "process-id")')" | tr -d ' ')
prlimit --pid "$P" --as=$((2 << 30))
```

To cap a whole run, start its server in a scope of its own:
`systemd-run --user --scope -p MemoryMax=4G -p OOMPolicy=continue fux
'(start)'`. `OOMPolicy=continue` keeps the scope when the OOM killer ends a
process in it, which is then the biggest, the pane that flooded, rather
than every process in it.

## Take a crashed server's panes back

A server killed with `kill -9`, or by the OOM killer, ends alone: its
panes' processes wait, with their programs, beside its socket. `(start)`
on that socket takes them back, as they were: their ids, names, screens
and programs. Its journal is new: a cursor from before is refused,
saying what was lost.

```sh
(
  export FUX_SOCKET="$PWD/crash/fux.sock"
  fux '(start)'
  fux '(rename (new-tab (argument (program (size (in (options) "work") 80 24) "sleep") "600")) "sleeper")' > /dev/null
  P=$(fux '(get (get (pane "sleeper") "program") "process-id")')
  kill -9 "$(fux '(get (status) "server-process-id")' 2>/dev/null || lsof -t "$FUX_SOCKET")"
  while fux '(status)' > /dev/null 2>&1; do sleep 0.1; done
  fux '(start)'
  fux '(listing (list-session))'
  [ "$(fux '(get (get (pane "sleeper") "program") "process-id")')" = "$P" ]
  fux '(kill-server)'
)
```

## A project with agent workspaces

A workspace holds tabs and workspaces. `in` again names a workspace in the
one before, made if it is not there, so each agent of a project gets a
workspace of its own under it, and the bar shows the path, `work › agent1`.
An operation on the project covers the workspaces in it: its `events`, `panes`,
`dump` and `kill` take in its agents, so a finished agent closes with one
`kill`, and `move` puts a workspace in another, or with `(all)` at the top.

```sh
fux '(new-tab (program (size (in (in (options) "work") "agent1") 80 24) "sh"))' > /dev/null
fux '(new-tab (program (size (in (in (options) "work") "agent2") 80 24) "sh"))' > /dev/null
fux '(length (panes (workspace "work")))'
fux '(get (get (workspace "agent2") "parent") "name")'
fux '(kill (workspace "agent2"))'
```

## An orchestrator's agents

An agent that starts agents gives each level a workspace: the fleet, each
orchestrator in it, and each of its tasks, where a task's implementer and
reviewers get a tab apiece, so the bar shows `fleet › orch1 › task1`. A
finished task closes with one `kill`, an orchestrator's `kill` stops all
its tasks, and the fleet, left empty, closes with its last child. Names
are one namespace on a server, so each task and agent has a name of its
own.

```sh
fux '(rename (new-tab (program (size (in (in (in (options) "fleet") "orch1") "task1") 80 24) "sh")) "task1-impl")'
fux '(rename (new-tab (program (size (in (in (in (options) "fleet") "orch1") "task1") 80 24) "sh")) "task1-review")'
fux '(rename (new-tab (program (size (in (in (in (options) "fleet") "orch1") "task2") 80 24) "sh")) "task2-impl")'
fux '(line (pane "task1-impl") "echo implementing")'
[ "$(fux '(length (panes (workspace "task1")))')" -eq 2 ]
[ "$(fux '(length (panes (workspace "orch1")))')" -eq 3 ]
fux '(kill (workspace "task1"))'
[ "$(fux '(length (panes (workspace "orch1")))')" -eq 1 ]
fux '(kill (workspace "orch1"))'
fux '(workspace "fleet")' || [ $? -eq 1 ]
```

## An editor beside switchable terminals

`stack` puts a new pane on another's rectangle, as `split` puts one beside
it: the two are children of a stack, and each client is shown one child
of a stack, its own. Every child keeps its size, so choosing one resizes no
program. `step` on a pane in a stack shows a client its next, previous or
last child (`C-b o`, `C-b q`), and `show` any pane. A tab's `nodes` lists a
stack as `("stack" (CHILD ...))`, which `arrange` takes too.

```sh
fux '(rename (new-tab (program (name (size (in (options) "work") 80 24) "code") "sh")) "editor")'
fux '(rename (split (program (axis (pane "editor") "horizontal") "sh")) "tests")'
fux '(rename (stack (program (pane "tests") "sh")) "build")'
fux '(data-text (get (first (get (snapshot (tab "code") "topology") "tabs")) "nodes"))'
[ "$(fux '(get (pane "tests") "columns")')" = "$(fux '(get (pane "build") "columns")')" ]
```

## Save and restore a layout

`dump` prints a grift program that makes the layout and its programs
again (never what the processes held); `fux --file` runs it, here on a
fresh server, its workspaces in the workspace its first argument names
(made if it is not there), else at the top. Saving, cloning and moving a
project are that one mechanism.

```sh
fux '(dump (workspace "work"))' > work.grift
(
  export FUX_SOCKET="$PWD/copy/fux.sock"
  fux '(start)'
  fux --file work.grift home
  fux '(get (get (workspace "work") "parent") "name")'
  kill "$(fux '(get (status) "server-process-id")' 2>/dev/null || lsof -t "$FUX_SOCKET")"
)
```

## Operations from a recipe file

Operations only some programs want are recipe files, in a checkout of fux at
`recipes/`, which a program loads with `require!` (relative to the file
or directory that names it) or runs with `fux --file`: `layouts.grift`'s
presets arrange a tab's panes, and `confirm.grift`'s `confirm` asks before
an action (the next recipe binds it).

```sh
fux '(split (program (axis (pane "shell") "horizontal") "sh"))' > /dev/null
fux '(require! "recipes/layouts.grift") (even-h (tab (pane "shell")))'
```

## Key bindings and config

The config is grift that each terminal runs as it attaches;
`fux '(list-keys)'` prints the bundled keys as config lines. `edit-history`
opens the focused pane's history in this terminal's `$EDITOR` (else `vi`)
at its last line, bundled on `C-b c`. A binding runs a code value,
`(code! (lambda! () ...))`, never a string; a name of the config's is defined before
the binding that names it. A key maps to a binding or to a map of more
keys: the prefix is a key that holds one, and `bind-key` takes keys from the
root. This config moves the prefix, and every key after it, to `C-a`,
binds `edit-history` on `C-a e` too, a function of its own on `C-a v`, and a
map `C-a g` of its own that repeats, so `C-a g h h h` moves a border
three cells. `escape-delay-ms` is one of the settings the README lists.

```grift config
; ~/.config/fux/config.grift
(move-keys "C-b" "C-a")
(set "escape-delay-ms" 35)
(bind-key "C-a e" (code! (lambda! () (edit-history (me)) #inert)))
(label "history in the editor")
(define! split-right (lambda! () (split (axis (focused (me)) "horizontal")) #inert))
(bind-key "C-a v" (code! (lambda! () (split-right))))
(bind-key "C-a g h" (code! (lambda! () (move-border (focused (me)) "left" 1))))
(repeat "C-a g")
```

Another program reads a client's keys, and the map it is in now, from
that client itself, and its preferences. Here a client of no terminal
says its id, and two of its keys, in the map `C-b` holds, and one
preference are read from it.

```sh
fux '(attach (size (options) 80 24))' > client.id &
A=$!
until [ -s client.id ]; do kill -0 "$A"; sleep 0.1; done
fux '(define! read (keys (client (first (arguments)))))
  (define! prefix (first (filter (lambda! (k) (same? (get k "key") "C-b")) (get read "keymap"))))
  (print (get read "at"))
  (filter (lambda! (k) (member? (get k "key") (list "d" "r"))) (get prefix "keymap"))' "$(cat client.id)"
fux '(filter (lambda! (p) (same? (get p "name") "bell")) (preferences (client (first (arguments)))))' "$(cat client.id)"
kill "$A"
```

A prompt's callback takes the line typed. `code!` keeps it a value until
Enter runs it, later and in an interpreter of its own, so the config's names
reach it and a local of the binding doesn't. What it needs from the key press
goes in a list beside it, and it takes those values before the line. This
binding asks for a line and types it into the pane focused when the key was
pressed.

```grift config
(bind-key "C-b p" (code! (lambda! ()
  (prompt (me) "command" ""
    (list (list (focused (me))) (code! (lambda! (target line-value)
      (line target line-value))))))))
(label "type a command")
```

A config's `require!` reads beside it: this one, beside a copy of fux's
`recipes`, asks before `C-b X` terminates the focused pane's command. The
list holds what the action needs and the code it runs: `confirm` calls the
code with those values alone.

```grift config
; ~/.config/fux/config.grift
(require! "recipes/confirm.grift")
(bind-key "C-b X" (code! (lambda! ()
  (confirm (me) "terminate its command?"
    (list (list (focused (me))) (code! (lambda! (target) (signal target "TERM"))))))))
```

## The bar on a row of its own

The bar takes no row by default: the panes have every row, the prefix
draws the bar over the bottom one until it ends, and a bell or a
program's notification shows in its corner for five seconds. This
config gives the bar its own row below the panes again.

```grift config
; ~/.config/fux/config.grift
(set "bar" (quote! ("bottom" ("workspace" "tabs") ("pane") #false)))
```

## A function of your own

`define!` makes a name for one command line; `require!` loads names
from a file, relative to the file or directory that names it.

```sh
fux '(define! screen-has (lambda! (p needle) (not? (empty-list? (search (string (capture p)) needle 0))))) (screen-has (pane "shell") "42")'
cat > names.grift <<'EOF'
; pane text -> boolean: whether the pane's screen shows the text
(define! screen-has (lambda! (p needle)
  (not? (empty-list? (search (string (capture p)) needle 0)))))
EOF
fux '(require! "names.grift") (screen-has (pane "shell") "42")'
```

## Read the dictionary

A program's dictionary is data: `word` gives a name's record, where it
came from (`core`, `fux`, `library`, or `run` for the program's own), its
effect, its documentation (all `(explain "NAME")` says of it), its definition
as it prints (`#[host NAME]` for a word in Rust), and `codes`, what
it can fail with as it runs (`none` where that is unknown);
`dictionary` gives every name's. `built-in-names` lists grift's own, and
`explain` shows a library operation's source. None of them asks the server.

```sh
fux '(get (word "line") "definition")'
fux '(get (word "capture") "documentation")'
fux '(get (word "kill") "codes")' | head -n 3 || [ $? -eq 141 ]
[ "$(fux '(define! double (lambda! (x) (multiply x 2))) (get (word "double") "provenance")')" = run ]
fux '(length (filter (lambda! (w) (same? (get w "provenance") "fux")) (dictionary)))'
fux '(explain "shows")'
```

## Run a grift program from a file or stdin

`fux --file FILE ARGS…` runs a file, its arguments in `(arguments)`, and `fux - ARGS…`
runs what stdin says, its arguments the same; a file can start `#!/usr/bin/env -S fux --file`. It prints
what the program prints, then its last value; a failure says why on stderr and
exits with its status. Neither needs a server unless the program asks one
for something. Without fux, grift's own `grift` runner runs the same language,
with no fux operations ([grift's README](https://github.com/gold-silver-copper/grift)).

```sh
printf '(define! square (lambda! (x) (multiply x x)))\n(square 7)\n' > square.grift
fux --file square.grift
printf '(add 3 4)\n' | fux -
echo '(arguments)' > args.grift && fux --file args.grift one two
echo '(arguments)' | fux - one two
echo '(divide 1 0)' | fux - || [ $? -ne 0 ]
```

## Keep data in a context

A context is grift's table of names, and a value like any other: `(context ())`
makes an empty one, `bind` sets a name in it, `lookup` reads one with a
default, and `names` lists them. fux's `unbind` takes keys, so grift's, which
removes a name, is read from `(standard-context)`. Lists hold everything else: `pair`, `first`,
`rest`, `element-at`, `take`, `drop`, `map`, `filter`, `fold` and `sort`.

```sh
fux '(define! counts (context ()))
  (define! count! (lambda! (name) (bind counts name (add 1 (lookup counts name 0)))))
  (for-each count! (quote! (a b a)))
  (list (lookup counts (quote! a) 0) (names counts))'
fux '(define! counts (context ()))
  (bind counts (quote! a) 1)
  (define! forget (lookup (standard-context) (quote! unbind) #inert))
  (forget counts (quote! a))
  (names counts)'
fux '(element-at (map (lambda! (n) (multiply n n)) (list 1 2 3)) 2)'
```

## Replace a function later

Names are looked up as a call runs, so a function sees the newest definition of
what it calls, and redefining one changes every caller. A value in a name is the
same: to replace behaviour, `define!` the name again.

```sh
fux '(define! greeting (lambda! () "hello"))
  (define! greet (lambda! () (print (greeting))))
  (greet)
  (define! greeting (lambda! () "goodbye"))
  (greet)'
```

## An interpreter in grift

grift's reader and evaluator are operations: `read-form` reads one form from a
text at an offset, giving the form, its start and its end, and `evaluate` runs
a form in a context. So a loop that reads a text form by form and evaluates each
is a few lines, and `vau!` makes a new form the way the library makes `when!`:
it gets its operands unevaluated, and the caller's context.

```sh
cat > interpret.grift <<'END'
; text context -> the last form's value: reads and runs each form of the text
(define! interpret (lambda! (source where)
  (define! walk (lambda! (from last-value)
    (let! ((remaining (string-trim (slice source from (byte-length source)))))
      (if! (same? remaining "") last-value
        (let! (((form #ignore end) (read-form source from)))
          (walk end (evaluate form where)))))))
  (walk 0 #inert)))
END
[ "$(fux '(require! "interpret.grift") (interpret "(define! square (lambda! (x) (multiply x x))) (square 7)" (current-context))')" = 49 ]
fux '(define! unless! (vau! (test . body) caller
    (if! (evaluate test caller) #inert (evaluate (pair (quote! sequence!) body) caller))))
  (unless! (same? 1 2) (print "no"))'
```

## Save a session as readable data

A dump is a grift program over a quoted record: read it before you run it.

```sh
fux '(dump (all))' > saved.grift
head -n 1 saved.grift
```

## Discover operations

`help` lists every operation, `explain` says one, and `list-keys` prints a
client's keys.

```sh
fux '(help)' | head -n 3 || [ $? -eq 141 ]
fux '(explain "split")'
fux '(list-keys)' | head -n 3 || [ $? -eq 141 ]
```

## Clean up

Close the workspace the recipes made, and with it their panes. A server
stops when its last pane closes.

```sh
fux '(kill (workspace "work"))'
```
