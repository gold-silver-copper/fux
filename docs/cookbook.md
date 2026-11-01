# The fux cookbook

Short recipes for what agents and people do with fux. Each is a heading,
what it's for, and a block to copy. The `sh` blocks run as they stand, top
to bottom, each in a shell of its own with `set -e -o pipefail`, against
the server `FUX_SOCKET` names: the second recipe makes the shell named
`shell` that later ones type into, and the last closes everything they made. A command expected to fail says
the exit status it expects, `|| [ $? -eq N ]`. A test runs every recipe,
so a recipe here is one this fux runs.

**The boundary:** the shell composes `fux` commands, fuxth does what must
happen inside one request or one key binding, and ordinary programs do
everything else.

fux has no files to read or write; the `fux` command is the same idea
spelled as commands, and a file is your own redirect:

| As a file, it would be | With fux |
| --- | --- |
| `cat screen` | `fux %2 capture` |
| `echo X > input` | `fux %2 str X line` |
| `tail -f events` | `fux all str '' events` |
| `cat events >> FILE` | `fux all str '' events >> FILE` |

## Pick a server

`FUX_SOCKET` names the server every `fux` command talks to. `fux start`
makes the socket's directory, only you allowed in (mode 0700), and
refuses one anyone else may open. A server of your own keeps your panes
apart from anyone else's; stop it with `kill`.

```sh
(
  export FUX_SOCKET="$PWD/mine/fux.sock"
  fux status > /dev/null 2>&1 || fux start
  kill "$(fux status str server_pid get)"
)
```

## A shell to type into

A new tab in workspace `work` running `sh`, its pane named `shell`: the
later recipes type POSIX `sh` into it, whatever your `$SHELL` is (leave
out `str sh program` for that one). From outside fux a new tab needs a
size; run inside a fux pane, it takes that pane's tab's.

```sh
fux opts str work in 80 24 size str sh program new-tab str shell rename
```

## Run a program and wait for it to end

Start it in a new tab, or in a split of a pane, and print its exit status.

```sh
fux opts str work in 80 24 size str sh program str -c arg str 'sleep 1; exit 3' arg new-tab 60 until-ended str exited get
fux str shell pane str vertical axis str sh program str -c arg str 'exit 5' arg split 60 until-ended str exited get
```

## Type a line, send keys

`line` types a line and presses Enter; `send-keys` sends text and named
keys (`fux help keys` lists the names).

```sh
fux str shell pane str 'echo hello' line
fux str shell pane str C-c key send-keys
fux str shell pane str 'echo hello again' text str Enter key send-keys
```

## Wait for text on the screen

Wait for a text only the output holds: the typed line shows `$((6*7))`,
never `42`. `shows` prints the screen it matched. Its 30 seconds bound
each wait for a change, not the whole: a screen that changes without the
text keeps it waiting. For a deadline on the whole, use `timeout(1)`
(below).

```sh
fux str shell pane str 'sleep 1; echo "built: $((6*7))"' line
fux str shell pane str 'built: 42' 30 shows string
```

## Wait for a quiet screen

`quiet` waits until nothing on the screen has changed for that many
seconds, then prints it. Quiet is a fact about the screen, not proof the
program is done.

```sh
fux str shell pane 2 quiet string
```

## Paste a file

The file's text goes to the program whole, as one paste (bracketed if the
program asked for it), however long it is. A paste into an attached
terminal arrives whole too.

```sh
printf 'echo pasted\n' > lines.txt
fux str shell pane input paste send-keys < lines.txt
```

## Read the screen and the history

As text: the screen, the last 100 lines of history and the screen, or all
of it.

```sh
fux str shell pane capture
fux str shell pane 100 history capture
fux str shell pane all-history capture
```

As JSON, one object a line, for `jq` (`fux help json` says every field):

```sh
fux str shell pane capture json | jq -r '.version, .lines[-1]'
fux ls | jq -r '.panes[] | [.id, .name // "", .state.exited // ""] | @tsv'
```

## What a pane runs, and where

fux gives the pane's process (`pid`) and its terminal's foreground
process group (`foreground_group`); the OS says what they run. The
group's leader may have exited while the rest of a pipeline runs on:
then `ps -p` fails, the leader's command and directory are unknown, and
`pgrep -g` still lists the group. Here a second pane, `piped`, runs a
pipeline whose leader `true` exits at once.

```sh
fux opts str work in 80 24 size str sh program new-tab str piped rename > /dev/null
fux str piped pane str '$' 30 shows > /dev/null
fux str piped pane str 'true | sleep 300' line > /dev/null
until G=$(fux str piped pane str foreground_group get) &&
  [ "$G" != "$(fux str piped pane str pid get)" ] && ! kill -0 "$G" 2> /dev/null; do sleep 0.1; done
```

```sh
for p in shell piped; do
  G=$(fux str "$p" pane str foreground_group get)
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
fux str piped pane kill
```

## Follow events from a cursor

`events` prints the journal's events, one JSON object a line, each with
its `next` cursor, until the scope is gone or the server stops. Take a
cursor before you act and you miss nothing in between; `head`, `jq` or
`timeout` ends it. When its reader stops early, fux ends quietly with
141, as `cat` does.

```sh
C=$(fux ls str cursor get)
fux opts str work in 80 24 size str true program new-tab > /dev/null
fux all str "$C" events | jq -nc 'first(inputs | select(.event == "exited"))' || [ $? -eq 141 ]
```

An empty cursor starts now; to go on after a stop, pass the last line's
`next`.

## Keep a journal file

The journal file is your redirect, written as long as the reader runs;
read it with `jq`, `grep` or `tail -f`. Here the loop waits until the
file holds the program's exit, failing if the reader ended (`kill -0`),
then stops the reader.

```sh
C=$(fux ls str cursor get)
fux all str "$C" events >> journal.ndjson &
J=$!
fux opts str work in 80 24 size str true program new-tab > /dev/null
until grep -q '"exited"' journal.ndjson; do kill -0 "$J"; sleep 1; done
kill "$J"
```

## Act only on the screen you saw

`expect` sends keys only if the pane is still at the version you read;
otherwise it sends nothing and fails `stale` (exit 1): read the screen
again.

```sh
V=$(fux str shell pane capture version)
fux str "$V" expect str 'echo yes' line || [ $? -eq 1 ]
```

## Stop a runaway with timeout

fux sets no time limits of its own: `timeout(1)` bounds a fux command
(exit 124 when it stops it), and a program in a pane.

```sh
timeout 2 fux str shell pane str 'never shown' 600 shows || [ $? -eq 124 ]
fux opts str work in 80 24 size str timeout program str 2 arg str sleep arg str 600 arg new-tab 60 until-ended str exited get
```

## Save and restore a layout

`dump` prints a fuxth program that makes the layout and its programs
again (never what the processes held); `fux --file` runs it, here on a
fresh server.

```sh
fux str work workspace dump > work.fux
(
  export FUX_SOCKET="$PWD/copy/fux.sock"
  fux start
  fux --file work.fux
  kill "$(fux status str server_pid get)"
)
```

## Key bindings and config

The config is fuxth that each terminal runs as it attaches;
`fux list-keys` prints the bundled keys as config lines. `edit-history`
opens the focused pane's history in this terminal's `$EDITOR` (else `vi`)
at its last line, bundled on `C-b c`. This
config moves the prefix to `C-a`, binds `edit-history` on `e` too, and
binds a word of its own on `v`.

```fux
# ~/.config/fux/config.fux
str C-a prefix
str e str 'me edit-history' bind  str 'history in the editor' label
define split-right me focused str horizontal axis split drop end
str v str split-right bind
```

## A word of your own

`define … end` makes a word for one command line; `require` loads words
from a file, relative to the file or directory that names it.

```sh
fux define screen-has swap capture string swap contains end str shell pane str 42 screen-has
cat > words.fux <<'EOF'
# pane text -- flag: whether the pane's screen shows the text
define screen-has swap capture string swap contains end
EOF
fux require words.fux str shell pane str 42 screen-has
```

## Run a fuxth program on its own

`fuxth` runs a fuxth program without fux: a file, or the program on its
stdin. It prints only what the program prints, then the values left, one
a line; a failure says why on stderr and exits 1, and Ctrl-C exits 130.
On a terminal with no file, `fuxth` is a REPL that prints the stack and
`ok` after each line; `rlwrap fuxth` gives it line editing and history.

```sh
printf 'define square dup mul end\n7 square\n' > square.fux
fuxth square.fux
printf '3 4 add\n' | fuxth
echo args > args.fux && fuxth args.fux one two
echo '1 0 div' | fuxth || [ $? -eq 1 ]
```

## Keep data in cells

fuxth's data space holds a value a cell. `variable` and `create` name
cells, `store-next` and `allot` add them, `fetch` and `store` read and
write one, and an address plus n is the cell n on. A defining word is
`create`, then `does` for what the words it makes do.

```sh
fuxth <<'EOF'
create squares 1 store-next 4 store-next 9 store-next
squares 2 add fetch print
variable count 0 count store 3 0 do 1 count add-store loop count fetch print
define table create allot does swap add end
3 table slots str hi 1 slots store 1 slots fetch print
EOF
fux 5 constant five five five mul
```

## Edit a file with fuxed

fuxed is a small screen editor written in fuxth, in a checkout of fux at
`fuxed/fuxed.fux`: `fuxth fuxed/fuxed.fux FILE` on a terminal. Its keys and
commands are in `fuxed/README.md`; M-x runs a line of fuxth in it, such as
`str w first ctrl str save bind`. Off a terminal it reads its keys from
stdin and the screen size from `LINES` and `COLUMNS`, so a file of keys
edits a file: here, type, Enter, type, C-s to save and C-q to quit.

```sh
printf 'hello\015world\023\021' > keys
LINES=24 COLUMNS=80 fuxth fuxed/fuxed.fux notes.txt < keys > /dev/null
[ "$(cat notes.txt)" = "$(printf 'hello\nworld')" ]
```

## Clean up

Close the workspace the recipes made, and with it their panes. A server
stops when its last pane closes.

```sh
fux str work workspace kill
```
