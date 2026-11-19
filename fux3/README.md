# fux3

A terminal multiplexer where a pane is a directory of ordinary files. One
small process, the **holder**, keeps each pane: it holds the program's
terminal and keeps the files current. There is no server and no socket.
After `fux3 start`, `cat`, `less`, `ls`, `printf` and `echo` do everything:
read, watch, list, type, resize and kill.

fux3 runs on Linux and macOS. It is beside fux and fux2 and shares no code
with them.

## Build

```sh
cargo build --release -p fux3 -p fux3-view    # target/release/fux3 and fux3-view
```

Keep `fux3-view` in the same directory as `fux3`: `fux3 view` runs it from
there.

## Example

```sh
D=$(fux3 dir)                               # the session directory
fux3 start build -- sh                      # prints the pane's directory, $D/main/build
printf 'echo hi\n' > "$D/main/build/in"     # type into it
cat "$D/main/build/screen"                  # line 1: the pane's id and version; then the screen
cat "$D/main/build/screen.styled"           # the same screen in colour
less "$D/main/build/history"                # what scrolled off, as plain text
cat "$D/main/build/status"                  # size, state and exit code, pids, modes, labels
echo "resize 100 30" > "$D/main/build/ctl"  # a command; a refusal appears in errors
echo "send $HOME/paste.txt" > "$D/main/build/ctl" # a send of any size up to 16 MiB, whole or refused
ls "$D"                                     # the workspaces: main
ls "$D/main"                                # its panes: build
fux3 view                                   # workspace main as fux shows it (see "The viewer"); C-b d detaches
fux3 view main --do "split build right"     # a layout command: an edit of the layout file, viewer or no viewer
mkdir "$D/rig" && mv "$D/main/build" "$D/rig/server"  # the live pane, moved and renamed: still the same pane
printf 'echo hi\n' > "$D/rig/server/in" && cat "$D/rig/server/screen"
echo kill > "$D/rig/server/ctl"             # ends the program and all it started; the directory goes
```

(Typed by hand, each `cat` shows what the line before did. A script is faster
than the program: the screen follows a write to `in` by a few milliseconds,
so wait for it with `fux3 wait` or a short poll.)

**The rule.** A pane is its directory and its id; a workspace is its
directory; a name is only where one is listed right now. Whoever acts on one
opens it once and stays on that object. A pane gets its name by one rename
once it is whole, and loses it by one rename before it is taken apart. A
layout command is an edit of the layout file, not a message to a process.

## The commands

| Command | What |
|---|---|
| `fux3 dir` | prints the session directory, making it if missing |
| `fux3 start [WORKSPACE/]NAME [OPTIONS] [-- PROGRAM ARGS...]` | starts a pane in WORKSPACE (default `main`, made if missing); prints its directory. The program defaults to `$SHELL`. Options: `--size COLSxROWS` (default: the terminal it is run in, else 80x24; at most 500x200), `--cell WxH` (a cell's size in pixels for pictures; default: the terminal's, else 10x20), `--no-pictures`, `--history BYTES` (default 4 MiB, 4096 bytes to 256 MiB), `--label KEY=VALUE` (up to 16), `--env NAME=VALUE` (a variable for the program, kept in its recipe), `--workspace-id DEV:INO` (refused with "workspace W was removed" unless the workspace's directory is that one, as `stat` numbers it: for a caller that named the workspace earlier, as the viewer does). With no program, the workspace's recipe of that name runs if there is one (see "Recipes"), else `$SHELL` |
| `fux3 ls` | lists the live panes, as `WORKSPACE/NAME` |
| `fux3 wait [WORKSPACE/]NAME [--ms N]` | waits until the pane's screen changes (default 10000 ms), prints its new line 1; exit 1 if nothing changed in time |
| `fux3 put [WORKSPACE/]NAME [TEXT]` | writes TEXT (else stdin; at most 512 bytes) to the pane's `in`: whole, or refused saying why (no pane, a dead holder, a program that has ended, input full), never waiting |
| `fux3 view [WORKSPACE] [--do LINE]` | the viewer of a workspace (below); with `--do`, one layout command's edit of the layout file, with or without a viewer running |

Every `fux3` command but `put` first clears the leftovers of panes whose holder is gone,
in every workspace. Pane and workspace names are 1 to 32 of `a-z 0-9 _ -`,
starting with a letter or digit; a pane's name is unique in its workspace.

## Workspaces

A workspace is a directory of the session, and a pane a directory of its
workspace: `$D/rig/build/`. `ls "$D"` lists the workspaces, `ls "$D/rig"` a
workspace's panes. A workspace is made when its first pane starts, or by
`mkdir "$D/rig"`, and has no file, process or state of its own but the
viewer's `.view/` (its layout) and `.recipes/`. An empty workspace stays until
you remove it (`rm -r "$D/rig/.view" "$D/rig/.recipes"; rmdir "$D/rig"`).

Ending a workspace is ending each pane in it:

```sh
for c in "$D/rig"/*/ctl; do echo kill 1<>"$c"; done
```

(`1<>` opens the FIFO to read and write, so the open never waits, even on a
dead pane's: see "Two hazards".)

### Moving and renaming a pane

`mv` moves or renames a live pane, within a workspace or between workspaces
of one session:

```sh
mv "$D/main/build" "$D/main/server"         # renamed
mv "$D/main/server" "$D/rig/"               # moved to workspace rig
```

A workspace is renamed the same way (`mv "$D/rig" "$D/rig2"`): its panes run
on, its recipes go with it, and a viewer showing it follows.

A moved pane is the same pane: the same id, the same program, every file kept current
where it is now, `in` and `ctl` still read, and `fux3 ls` lists it under its
new name. The holder works through its directory, never through a path. What
follows from "a name is only where it is listed":

- **The id is the pane.** `id` in its directory and line 1 of `screen` hold
  it; `status` names no workspace and no pane. A program finds its own pane,
  wherever it was moved, with `grep -lx "$FUX3_PANE_ID" "$FUX3_DIR"/*/*/id`.
  `FUX3_WORKSPACE`, `FUX3_PANE` and `FUX3_PANE_DIR` say where the pane was
  **when it started**, and are not changed by a move.
- **The recipe stays** under the name the pane started with: move it too if
  you want `fux3 start` to find it (`mkdir -p "$D/rig/.recipes" && mv
  "$D/main/.recipes/build" "$D/rig/.recipes/server"`).
- **Names are not checked on a move.** A pane moved to a name outside the
  rule (`Build`, `.x`) lives on but is not listed by `fux3 ls` or the viewer
  and is never cleared when it dies: move it back. The 128-pane limit is
  checked when a pane starts, not when one is moved in.
- **`mv a b` onto an existing pane `b` nests**: `a` becomes `b/a`, as `mv`
  does with any directory. `a` lives on, unlisted; `mv "$D/main/b/a"
  "$D/main/a"` brings it back. A kill of `b` removes only what `b`'s holder
  made, never `a`: `b`'s directory then stays, without its name
  (`.gone-ID`), until `a` is moved out or ended and a `fux3` command runs.
- **`mv` to another filesystem is a copy and a delete.** The copy is dead
  files: nothing reads its `in`, and `fux3 put` refuses it. The delete
  removes the live pane's directory while its holder and program run on,
  listed nowhere: end them with `kill "$(sed -n 's/^holder //p'
  COPY/status)"` (a holder's SIGTERM is its `kill`). A session directory is
  one filesystem; this happens only when moving a pane out of the session.
- **The viewer** finds a tile's pane by id: see "The viewer".

A program in a pane finds itself in its environment: `FUX3_DIR` (the
session) and `FUX3_PANE_ID` (the id on line 1), which stay true; and
`FUX3_WORKSPACE`, `FUX3_PANE` (its name) and `FUX3_PANE_DIR` (its
directory), true when it started.

The session directory is `$FUX3_DIR` if set (an absolute path), else
`$XDG_RUNTIME_DIR/fux3` on Linux and `~/Library/Caches/fux3` on macOS. It and
everything fux3 makes in it are private to you (0700 and 0600); fux3 refuses
a session directory anyone else can open (what is in it, a workspace made
with `mkdir` among it, is behind that directory whatever its own mode). Keep it under directories only you
can change (as `$XDG_RUNTIME_DIR` and your home are): fux3 checks the
directory itself, not its parents, and not ACLs on macOS. A holder's command
line (`ps`) is `start`'s own: the pane's name, its options (labels among
them) and its program, as any program's arguments are shown; `--env` values
are not on it.

## The pane directory

| Entry | What it is |
|---|---|
| `in` | FIFO: bytes for the program, as typed. One write of at most 512 bytes arrives whole; for more use `send` |
| `ctl` | FIFO: commands, one line each (below) |
| `screen` | line 1, then the screen as plain text, a line per row |
| `screen.styled` | the same line 1, then the screen with colours and attributes, then one `picture …` line per picture |
| `status` | one fact a line: `KEY VALUE` |
| `history` | plain text of the lines that scrolled off the screen, appended |
| `history.1` | the history before `history` last reached half its cap (there once it has) |
| `errors` | the last 32 refusals, a line each |
| `images/N.sixel` | one picture the program drew, as sixel: `cat` shows it in a sixel terminal |
| `exit` | there once the program has ended: its state line (`state exited 3`) |
| `id` | the pane's id (16 hex digits) and a newline, there from before the pane has its name: what makes a directory a pane's |

**Line 1** of `screen` and `screen.styled` is the same in both:

```
id=3f9a04c2b18e7d65 version=42 size=80x24 cursor=3,8 cursor-shown=yes
```

`id` is new every time a pane starts, so a new pane under an old name is told
apart. `version` grows with every change to the screen: output, a resize, the
program's end. The two files are replaced one after the other, so compare
their line 1 if you read both. Each file is replaced whole (written aside,
then renamed): you never read half a screen. Rows and columns count from 0.

**`screen.styled`** is made from the terminal's cells, never from the
program's bytes: the only escape codes in it set colours (SGR). `cat` shows it
correctly in a terminal and cannot set your title, move your cursor or make
your terminal answer anything. Each picture shown is a line after the rows:
`picture 1.sixel at=ROW,COL cells=COLSxROWS cropped=K cell=WxH`. ROW is
negative once the picture's top rows have scrolled into history; a picture
with no row left on the screen is no longer listed and its file goes. K
counts top rows cut off for good by a scroll region or the alternate screen.
Writing text over any cell of a picture removes the whole picture.

**`status`**:

```
id 3f9a04c2b18e7d65
version 42
size 80 24
cell 10 20
pictures on
state running
pid 4120
holder 4117
keys legacy cursor=normal keypad=normal other-keys=off backarrow=off
bracketed-paste on
newline-mode off
focus-reports off
mouse none default
alternate-screen off
label role worker
commands 3
refused 1
queued 0
```

`state` is `running` (`starting` for an instant before the program is
started), `exited CODE`, `signalled NAME`, or `kill-incomplete WHAT`. `pid` is the program's (`none` once it is gone), `holder` the holder's.
`keys` and the mode lines say how the program asked for its input. `commands`
counts the lines `ctl` has taken, refused ones included; `refused` those
refused; `queued` the bytes waiting for the program. When writing the files
failed (a full disk), a last line says `publish failed: WHY`; the last good
files stay.

**`history`** is plain text, never the program's raw bytes: raw bytes shown in
a terminal replay escape codes, make that terminal answer the program's
queries into your shell, and let a pane's program attack whoever tails it. A
soft-wrapped line is one line. `less +G` or `tail -F` reads it.

`history` is **records**: a record is a line with its newline, at most
min(cap/2, 64 KiB) bytes (`--history` sets the cap, default 4 MiB, so 64 KiB
unless the cap is under 128 KiB). A line longer than that comes as pieces,
each a record ending ` [fux3: continued]`; taking that mark and the newline
after it out gives the line back:

```sh
cat history.1 history | perl -pe 's/ \[fux3: continued\]\n//'
```

Only whole records are written, so a read never ends inside one, and a line
still being printed (no newline yet) appears once it ends or once it is long
enough for a piece. Each file holds at most half the cap: a record that does
not fit starts a new file, `history` being renamed `history.1` (the older
`history.1` goes). So rotation falls only between records. `tail -F history`
follows the new file and repeats nothing; it sees every record once as long
as it looks again before the new file fills too (it looks once a second):
under a flood with a small cap a whole generation can come and go between
two looks, and is then only in `history.1`, or gone. `tail -f` stops at the
old file. `cat history.1 history` is everything kept, in order, under the
cap; what neither holds is gone. When the program ends with the start of a
wrapped line scrolled off and its rest still on the screen, that start is
written as a piece. A resize takes what scrolled off before it; lines the
resize itself pushes off the screen are not added.

## `ctl`

| Line | Effect, as `status` shows it | Refused: a line in `errors` |
|---|---|---|
| `resize COLS ROWS` | `size COLS ROWS`, a new `version` | bad numbers, or the terminal refused: nothing changed |
| `send PATH` | the file's bytes go into the program's input queue whole, as typed; `queued` grows, then drains (a program that ends first gets what it read; the rest is dropped, noted in `errors`) | not an absolute path, a link, not a regular file, not yours, over 16 MiB, the queue full, the program ended: nothing was sent |
| `paste PATH` | the file's text as one paste, as a terminal pastes: line breaks become CR, and it is bracketed if the program asked for that; otherwise as `send` | as `send`, or not UTF-8 |
| `signal NAME` | the signal (`INT`, `TERM`, `HUP`, …; one leading `SIG` is dropped) goes to the terminal's foreground process group, else the program's | no such signal, the program ended |
| `cell W H` | `cell W H`; pictures drawn after it use it | smaller than 2x6 or over 1000 |
| `kill` | the program and everything it started end, and the directory goes | `kill-incomplete WHAT` in `state` and in `errors`: send `kill` again |

Once the program has ended, every command but `kill` is refused. Blank lines
are ignored and not counted.

A refusal reads `N LINE: WHY`, where `N` is the command's number, the value
of `commands` once it was taken. **Telling your command's outcome:** read
`commands` (say 6), write your line, wait until `commands` is 7 or more, then
look for a line starting `7 ` in `errors`: there is one only if it was
refused. This holds for one writer with one command at a time: with several
writers of `ctl` at once, `N` is the holder's order, not yours; and compare
`refused` too, as `errors` keeps 32 lines. `fux3 wait` returns on a screen
change only, and a refused command changes no screen: poll `status`.

`send` reads the whole file before anything is sent, so typing on `in` and
other sends never land inside it. The path is the rest of the line, spaces
and all. The file is read in the holder's loop: a file on a stalled network
mount stalls the pane until it is read.

What is typed into `in` is never read as a command: pasted text or a
program's echo cannot resize or kill a pane.

## Waiting for a change

`fux3 wait build` returns when `screen` changes. On Linux,
`inotifywait -e moved_to "$D/main/build"` does the same with no fux3. Or poll line 1.

The screens are published at once on the first change after a rest, at most
every 40 ms while output goes on, and once more within 40 ms when it rests.
Measured on Linux (an Apple M2 Max laptop, Arch Linux, tmpfs): a key written to `in` shows in
`screen` 2.2 ms later (median, 3.8 ms worst of 30); a pane printing without
end (`yes`) costs about 100 file writes a second (25 publishes of `screen`,
`screen.styled`, `status` and a `history` append), whatever the program does.

## When the program ends

`status` says `state exited CODE` (or `signalled NAME`) and the last screen
stays readable until you `kill` the pane. Bytes written to `in` afterwards
are read and dropped, counted in one line of `errors`, kept last and
updated (`in: 8 bytes dropped in all: the program has ended (exited 3)`), so
drops never push a command's refusal out; `in` itself stays,
so a shell's `> in` never makes a regular file there.

Once the end has settled and the last screen is written, the directory gains
`exit`, holding the state line (`state exited 3`), written aside and renamed
in, so never seen empty or half-written. "Has it finished?" is
`[ -e "$D/main/build/exit" ]`. A waiter also checks the directory is still
there, since a kill removes it:

```sh
p=$D/main/build; while [ -d "$p" ] && [ ! -e "$p/exit" ]; do sleep 0.1; done; cat "$p/exit"
```

(A holder killed outright writes no `exit`, and its directory stays until the
next `fux3` command: see "Two hazards".)

## Recipes

Each `fux3 start` writes the pane's recipe, `WORKSPACE/.recipes/NAME`: one
executable POSIX `sh` script that starts the program again exactly, in the
same directory, adding the variables named by `--env` to the environment it
runs in, every word single-quoted so any byte comes back as it was. It holds
those variables and no others (never the environment `start` ran in, which
can hold secrets); run, it inherits the environment of whoever runs it, as
the first start inherited `start`'s:

```sh
#!/bin/sh
# fux3 size 120x40
# fux3 label role=tests
cd -- '/home/me/src' || exit 126
RUST_LOG='debug'; export RUST_LOG
exec 'cargo' 'watch' '-x' 'test'
```

The two comment lines keep the start's size and labels: a start from the
recipe has them again unless `--size` or `--label` say otherwise, so a
template's tab that matches on a label finds the pane it started. A program
whose name starts with `-` is run through `env` (`exec env -- '-x'`), as
`exec` would read it as an option: such a program replays only where `env`
is on `PATH`, and not if its name holds `=`. A name `sh` keeps for itself
(`PPID`, `UID`, `EUID`, `BASHOPTS`, `SHELLOPTS`, `BASH_VERSINFO`) is refused
by `--env`, as its recipe could not run. The recipe is written last, once the
pane runs: a start that fails leaves the recipe of the last good one.

It is written aside and renamed in, at most 64 KiB (a start whose recipe would
be larger is refused), and it lives outside the pane's directory, so a kill
and the clearing of dead panes leave it. `fux3 start NAME` with no program
runs it again; `cat "$D/main/.recipes/NAME"` shows what it would run, and
`sh "$D/main/.recipes/NAME"` runs it in your own terminal. A recipe stays
after its pane is gone, until the name starts another program; a file at the
name that is not a plain file of at most 64 KiB is refused, never skipped. A
start that runs a recipe takes no `--env` (the recipe sets its own: give the
program again to change them).
Starting a
workspace's every recipe:

```sh
for r in "$D/rig/.recipes"/*; do fux3 start "rig/${r##*/}"; done
```

## Kill

`echo kill > "$D/WORKSPACE/NAME/ctl"` hangs up the program, then kills its session; on
Linux the holder is the subreaper of everything the program started, so a
`setsid` child or a double-forked daemon is found and killed too.

**Panes started inside a pane are part of it.** One kill ends a task and
everything it started, panes included: on Linux, a `fux3 start` run inside a
pane makes a holder that pane's holder adopts, and the outer pane's kill asks
it to end (SIGTERM, which to a holder is its own `kill`), so the inner pane
takes itself apart: its program ends, its directory goes. An inner holder
that has not ended in a second is killed outright, and the outer kill then
clears its directory itself. Either way no inner directory outlives the
outer kill, and no later command is needed. A viewer run inside a pane
starts its shells inside that pane, so killing that pane ends them. On macOS
the inner pane's holder left the outer pane's session and cannot be found,
so it outlives the outer pane: end it with its own `ctl`.

`kill HOLDER` (SIGTERM to the holder, whose pid is `holder` in `status`) is
the same as `kill` written to `ctl`.

Once the processes are gone (which takes up to a second when a pane
started inside does not end at once: until then the pane keeps its name and
its `state`), the kill takes the pane's name away, by one rename (to
`.gone-ID` in its workspace; `.gone-ID-N` if a copy of the pane has that),
and only then stops writing and removes the
files: the name is free for a new pane from that rename on, however slow
the disk, and a holder that dies in between leaves a nameless directory the
next `fux3` command clears.
Only the files a holder makes are removed: anything else you put in a
pane's directory stays, in that nameless directory. When something could not
be killed, the directory keeps its name, `state` says `kill-incomplete` and
what is left, and you may send `kill` again. On macOS a process that left
the program's session and was re-parented away cannot be found, and is not
reported.

An inner holder that had to be killed outright leaves, for the moment until
the outer kill clears it, a FIFO nobody reads; and one whose pane is in
another session, or was moved into another pane's directory, is not found
by that clearing at all (the other session's next `fux3` command clears the
first; the second waits to be moved back). Ending every pane, without ever
waiting on a dead one:

```sh
for c in "$D"/*/*/ctl; do echo kill 1<>"$c"; done
```

## Two hazards of plain files

**A holder killed outright from outside** (`kill -9`, the kernel out of
memory) can clean nothing up: its directory stays, with its name, until the
next `fux3` command clears it. Until then `ls "$D/main"` shows it, its
`status` still says `running`, it has no `exit`, its program runs on if it
ignores the hangup it gets when its terminal goes, and **a plain write to its
`in` or `ctl` blocks for ever**: a FIFO with no reader blocks its writer in
`open`. Checking first is no guard: a holder killed between the check and
the write still hangs the writer. Two ways are safe:

- **`fux3 put`** for `in`: it opens without waiting and refuses at once,
  saying why (no pane, a dead holder, a program that has ended, input full).
  It opens the pane once and asks everything of that directory, so a new
  pane that takes the name meanwhile is never typed into.
- **Any `fux3` command first** (`fux3 ls`): it clears the dead, so their
  FIFOs are gone and a write fails ("No such file") rather than blocks. A
  holder killed after it still blocks: for a script that must never hang,
  open the FIFO to read and write, which never waits:

```sh
printf 'echo hi\n' | fux3 put main/build              # in: whole, or refused
echo kill 1<>"$D/main/build/ctl"                       # ctl: the open never waits; a dead pane takes nothing
for c in "$D"/*/*/ctl; do echo kill 1<>"$c"; done      # kill every pane; waits on no dead one
```

`1<>` on a dead pane's FIFO succeeds and the line goes nowhere: right for
`kill`, and the reason `in` wants `fux3 put`, which says so. What never
waits is the open. The write of a line waits only while the FIFO is full
(64 KiB of commands nobody has read), which a live holder does not leave
and a dead pane's FIFO cannot reach with a few kills. With plain
tools only, the same open as `put`'s in one line of Perl, which Linux and
macOS both ship:

```sh
printf 'echo hi\n' | perl -e 'use Fcntl; read(STDIN, my $t, 513); length $t <= 512 or die "over 512 bytes\n"; sysopen(F, shift, O_WRONLY|O_NONBLOCK) or die "$!\n"; syswrite(F, $t) == length $t or die "$!\n"' "$D/main/build/in"
```

Both refuse at once when nobody reads `in` ("No such device or address") or
the pane is gone, and write at most 512 bytes whole. Both read their text from
stdin to its end first, so they wait for whatever writes it. `fux3 put` takes
no lock and clears no dead pane: it never waits.

A holder ended any other way (its `kill`, SIGTERM, an outer pane's kill)
takes its directory with it.

**A write over 512 bytes to `in`** may arrive in pieces, mixed with another
writer's. Use `send` for anything long.

## With plain tools

Things fux3 needs no code for, each with its limit:

- **Viewing from elsewhere.** One open of `screen` is one whole frame, so
  `ssh host cat "$D/main/build/screen"`, `scp` and `rsync` copy whole frames. A
  mirror copies to a temporary file, then renames it, or its reader can catch a
  half-written copy. FIFOs do not cross a network mount: type with `ssh host
  fux3 put …`.
- **A screen bookmark.** `ln "$D/main/build/screen" "$D/saved"` keeps that
  frame, even after the pane is gone (the next publish replaces the name, not
  the file). A hard link stays on one filesystem (the session is a tmpfs on
  Linux): elsewhere, `cp` to a temporary file beside the destination, then `mv`.
- **Staying on one pane.** `cd "$D/main/build"` and use relative paths: they
  follow the pane when it is renamed or moved, and after a kill and a new pane
  under the same name, the old directory's files are gone and nothing you
  write reaches the newcomer.
- **Write-protecting a pane.** `chmod u-w "$D/main/build/in" "$D/main/build/ctl"`
  refuses every new open for writing at once; a writer that already has one
  open (a viewer that has typed) keeps it until it closes it. `chmod u+w` gives
  them back.
- **"Is it alive?"** Its directory's lock is held:
  `perl -e 'use Fcntl qw(:DEFAULT :flock); sysopen(D, shift, O_RDONLY) or die "gone\n"; print flock(D, LOCK_EX|LOCK_NB) ? "dead\n" : "alive\n"' "$D/main/build"`.
  A sample, not a guard: it may be dead a moment later (use `fux3 put`).

## Pictures

Sixel pictures are on by default. The holder lays them out in the cell size
`start` found (the terminal it was run in, else 10x20 pixels; `--cell` or
`ctl`'s `cell W H` change it), keeps at most 16 MiB of pixels, and writes
each picture shown once as `images/N.sixel`, re-encoded from its pixels; the
file goes when the picture is no longer shown. New pictures are written at
most 4 Mi pixels a second (16 Mi at once), so a program drawing without end
costs a bounded number of bytes. `--no-pictures` takes none.

## Where the files live, and macOS

On Linux the session directory is in memory (`$XDG_RUNTIME_DIR` is a tmpfs).
On macOS `~/Library/Caches/fux3` is on disk. An idle pane writes nothing; a
busy one rewrites its three screen files (a few KiB each) up to 25 times a
second and appends to `history` what scrolls off, as fast as the program
prints (measured under `yes`: about 3 MB a second, within the history cap's
rewrites). To keep that off the disk on macOS, put the session in a RAM disk
(no root needed):

```sh
dev=$(hdiutil attach -nomount ram://131072)        # 64 MiB
diskutil erasevolume HFS+ fux3 "$dev"               # mounted at /Volumes/fux3
mkdir -m 700 /Volumes/fux3/s && export FUX3_DIR=/Volumes/fux3/s
```

## Limits

At most 128 panes a workspace, 500x200 cells a pane, 16 MiB a `send`, 16 MiB and 64 KiB
of typing waiting for a program, 512 bytes a `ctl` line, 64 KiB of the terminal's
replies waiting (more are dropped, noted in `errors`), 32 lines of `errors`,
8 MiB a screen file, 64 pictures and 16 MiB of their pixels, 16 MiB a
picture's file (a larger one is not written, and the styled screen naming it
waits). Everything past
a limit is refused out loud, in `errors`, on stderr or in `status`.

## The viewer

fux3's viewer: it shows one workspace of a fux3 session the way fux shows its
own, and sends your keys to the focused pane. It reads and writes only the pane
files (see `fux3/DESIGN.md`) and the workspace's `.view/` directory. It owns
nothing: kill it however you like and every pane goes on. Start two on one
workspace and they show the same layout; each keeps its own focus and shown
tab.

```sh
D=$(fux3 dir)
fux3 start build -- sh
fux3 view                                       # workspace main; runs fux3-view with FUX3_DIR set
fux3 view main --do 'zoom build'                # a layout command: an edit of main's layout file
fux3 view rig                                   # workspace rig, made if missing
```

From another terminal, with or without a viewer of `main` running:

```sh
fux3 view main --do "split build right"         # every viewer of main shows it within a second
```

`fux3 view [WORKSPACE]` shows WORKSPACE (default `main`, made if missing, as
`fux3 start` does). The workspace keys go over the session's workspaces, the
directories of `$D`, by name: next, previous, a list, new (a name at a prompt:
the directory is made and a shell started in it) and close (asks, then ends
each pane, waits for them to go, and removes the directory, `.view` and all;
it refuses, removing nothing, if a pane is still starting or ending).
A viewer whose workspace's directory goes shows the first one left, else a new
`main`. Each viewer remembers, for each workspace it has shown, the tab and
focus it left there. Workspaces have no order but their names'.

**A tile belongs to a pane's id.** Rename a pane (`mv "$D/rig/a" "$D/rig/c"`)
and its tile, the focus and your keys stay with it; the layout file takes the
new name at the next layout command (until then a renamed pane that ends
shows its old name on its `gone` tile). `NAME@ID` in a layout command finds
the pane by its id, whatever it is called now. A bare tile (`pane NAME`) is a
name: it shows whichever pane has that name. Move a pane to another workspace
and its tile here stays, marked `NAME gone`, until you close it; in the other
workspace it is placed like any pane with no tile.

**A removed workspace.** A viewer, a `--do` and a confirmation each act on
the workspace's directory as it was when they began. If it was removed
meanwhile they fail with `workspace W was removed`, even when another
workspace has the name by then (nothing meant for the old one lands in the
new one); a viewer then shows the workspace that has the name now, else the
first one. A pane a layout command starts goes to that same directory.

### What it reads and writes

Paths are in the workspace shown, `$D/WORKSPACE/`.

| Path | What the viewer does with it |
|---|---|
| `NAME/screen.styled` | drawn in the pane's tile; looked at every 20 ms while shown, and read again only when replaced (its inode, size or time changed) |
| `NAME/status` | the pane's id, labels, state, size, key mode and newline mode; looked at the same way |
| `NAME/images/N.sixel` | the pictures the screen names: one whole in its tile is written as it is, at its place, when this terminal's DA1 says it draws sixel, if the file is one sixel alone (it starts with `ESC P` and holds no `ESC` but its closing `ESC \`); else its cells stay blank |
| `NAME/in` | your keys, encoded for the mode `status` gives (`keys`); Enter is CR LF under `newline-mode on`; at most 512 bytes a write, queued (64 KiB at most) while the pipe is full |
| `NAME/ctl` | `resize COLS ROWS` (only the fit step below), `paste PATH`, and whatever your grift sends (`kill`, `signal NAME`) |
| `NAME/history.1`, `NAME/history`, `NAME/screen` | copied into one text for `C-b c` (below) |
| `NAME/id` | which pane a directory is: a tile's pane is found by it after a rename |
| `.view/layout`, `.view/layout.lock` | the workspace's layout, shared by every viewer of it (below) |
| `.view/paste-PID-N` | a paste's text, 0600, removed 10 s after the `paste` command, or when the viewer leaves (a paste under 1 s old gets the rest of that second first) |

The workspace's directory is listed again once a second and whenever the
layout changes. A pane whose directory is gone keeps its tile, drawn as `NAME gone`;
one whose program ended keeps it too, marked `NAME exited 0` (or `signalled
…`). A new pane under an old name gets its own tile: tiles name a pane with its
id, unless the tile is bare (below). Panes the viewer starts get the first free `PREFIX-N` that no pane and no
tile uses.

### The layout file

`.view/layout` holds the workspace's tabs, and per tab a tree of splits with
relative shares over panes named `NAME@ID` or by a bare `NAME`, a zoom, and a
filter:

```
fux3-layout 2
version 7
next 4
tab t2 "all"
  zoom build@4673139adda1fbac
  split columns
    500 pane build@4673139adda1fbac
    500 split rows
      1000 pane shell-1@9d44f7110465cf2e
      1000 pane logs@9184f466b86ac9a9
tab t3 "builds"
  match "role=build"
```

A bare tile, `pane NAME` (and `zoom NAME`), is whichever pane has that name
now: it shows `NAME gone` while there is none, and each new pane of that name
takes it, unless that pane has a `NAME@ID` tile of its own. Commands that name
a pane act on a bare tile as on any other and leave it bare; tiles the viewer
adds are always `NAME@ID`.

A pane with no tile of its own is shown in the first tab whose filter it
passes (every `KEY=VALUE` is one of its labels; `name=NAME` is its name; no
filter takes any), else in a tab `other` at the end.
Every change is one command applied to the file as it is then, under the lock
(waited for 2 s at most), and written whole by atomic replace; `version` goes
up by one. A missing (after one was read), oversized (over 64 KiB) or wrong
file is refused out loud: the viewer keeps showing the last good layout and
says why in the bar's corner, and no command writes over it. `reset` is the
one command that does.

### Layout commands

One line each. A layout command is an edit of one workspace's layout file,
made under its lock, not a message to a viewer: run it with `fux3 view
[WORKSPACE] --do 'LINE'` (with or without a viewer running; it prints what it
did; a refusal goes to stderr, exit 1), or from grift with `(arrange LINE)`
(the workspace shown). Every viewer of the workspace shows the edit within a
second.

Before this, a line was written to a FIFO a viewer read; now:

```sh
# before: echo "split build right" > "$D/main/.view/ctl"   (needed a viewer reading; refusals in .view/errors)
fux3 view main --do "split build right"          # now: an edit of the file; the refusal is stderr and exit 1
```

`.view/ctl`, `.view/ctl.lock` and `.view/errors` are gone, and so is the grift
`layout-command`. `PANE` is `NAME` or `NAME@ID`, a pane of that
workspace, or `NAME@`, a bare tile no pane has (closing it takes only that tile); `TAB` is `tN` (the tab with that id, if there is one) or a title no
other tab has; `DIR` is `left`, `right`, `up` or `down`; a title with spaces is
quoted, `"like this"`. A quoted word is never a keyword: `tab new "match"` makes
a tab called match, and `"t1"` is the tab titled t1.

| Command | What it does |
|---|---|
| `split PANE DIR` | starts a shell (`shell-N`, your `$SHELL`) and puts it beside PANE on side DIR |
| `split PANE DIR NEW` | puts the existing pane NEW beside PANE on side DIR |
| `close PANE` | takes PANE's tile away (a pane still running comes back where its filter routes it) |
| `move PANE DIR` | swaps PANE with the pane beside it that way |
| `move PANE earlier` / `later` | swaps PANE with the one before or after it in its tab |
| `move PANE tab TAB` / `tab new` | moves PANE into a tab, or a new tab after its own |
| `border PANE DIR CELLS` | moves PANE's border toward DIR by CELLS cells (as seen in the screen it was given in) |
| `zoom PANE` | shows PANE alone in its tab, or restores the tab |
| `tab new [TITLE] [after TAB] [match KEY=VALUE…]` | a new tab |
| `tab rename TAB TITLE`, `tab close TAB`, `tab move TAB earlier\|later` | |
| `reset` | the default layout (one tab `all`), over whatever is there |

What a command did is printed as lines:
`version N`, `made tN`, `started NAME@ID`, and `changed NAME@ID` for
every pane whose tile moved or changed size. It reports geometry only: a pane
moved to another tab where its tile has the same place and size is not listed.

**Sizes.** Opening a viewer, moving the focus and resizing the viewer's
terminal resize no pane: a tile clips or pads it. The shipped commands end by fitting the
panes they changed, once (`resize COLS ROWS` on each one's `ctl`, the size of
its tile in the viewer that ran the command): that step is the library's
`edit!`. `C-b f` fits the focused pane by hand.

`--do` knows no viewer's size, so it fits no pane to its tile. A running
viewer that sees an edit from outside (at its once-a-second look) fits the
panes whose tiles the edit changed; with no viewer running nothing is fitted:
send `resize` to the `changed` panes yourself, or press `C-b f` later. With
two viewers of different sizes on one workspace, each fits, and the last one
to look sets the size.

### Templates

A workspace's skeleton is its `.recipes/` (one `sh` script per pane, which
the core writes when a pane starts) and a `.view/layout` with bare names. To
turn the running workspace `rig` into a template `$T`:

- A recipe stays after its pane is gone, so starting every recipe starts
  closed panes again too. Remove the recipes of panes the template should not
  start, among them any history pager's (`history-N`; a pager that ends by
  itself removes its own).
- The layout may hold two tiles of one name (an ended life's beside the new
  one's). Close the one you don't want (`fux3 view rig --do 'close NAME@ID'`)
  before stripping the ids: the reader refuses two `pane NAME`.

Then copy the recipes and strip the ids from the layout:

```sh
T=~/templates/rig; mkdir -p "$T/.view"
cp -Rp "$D/rig/.recipes" "$T/" && rm -f "$T/.recipes"/history-*
sed -E 's/^( +([0-9]+ )?(pane|zoom) [^@ ]+)@[0-9a-f]{16}$/\1/' "$D/rig/.view/layout" > "$T/.view/layout"
```

To start it as the workspace `new`, copy the skeleton in and start every
recipe (`fux3 start NAME` with no program runs the recipe of that name):

```sh
mkdir "$D/new" && cp -Rp "$T/.recipes" "$T/.view" "$D/new/"
for r in "$D/new/.recipes"/*; do fux3 start "new/${r##*/}"; done
```

Each pane takes its tile as it starts; no rebinding step. A recipe keeps its
start's size and labels, so a pane started again from it passes the same
tab filters (a tab with no filter takes every pane: put tabs that `match`
before it).

### Keys

fux's keys, wherever fux3 has the command. `C-b` opens the prefix menu, drawn
from the bindings alone (each has a caption and a heading); a key bound to a
group opens that group's menu, and Escape closes it. `C-b` twice sends `C-b`.

| Keys | |
|---|---|
| `C-b v` / `C-b s` | split side by side / stacked (a new shell) |
| `C-b x`, `C-b z`, `C-b c`, `C-b f` | close pane (asks; ends it, and its tile goes once it is gone), zoom or restore, history and screen in your editor, fit the pane to its tile |
| `C-b h j k l`, `C-b o`, `C-b q` | focus left/down/up/right, next pane, last pane |
| `C-b n`, `C-b b` | next / previous tab |
| `C-b t n h l g r x`, `C-b t m h l` | tabs: new, previous, next, choose, rename, close; reorder |
| `C-b w n h l g x` | workspaces: new, previous, next, choose, close |
| `C-b r h j k l` | move a border by a cell |
| `C-b m h j k l` | move the pane |
| `C-b a x k t n`, `C-b a m h l` | pane actions: close, terminate (`signal TERM`), move to tab…, to a new tab; reorder |
| `C-b e`, `C-b ?`, `C-b d` | grift prompt, every key, detach |

Mouse: a click focuses a pane. Mouse events are never sent to programs. A paste goes to the focused pane through
`paste` (bracketed if the program asked).

**Copying.** `C-b c` opens the focused pane's history and then its screen as
one text, at its end, in a pane below: `$EDITOR` if set, else `$PAGER`, else
`less +G` (`vi`, `vim`, `nvim`, `nano` and `emacs` are opened on the last
line; any other editor is given the file alone). Search and copy with the
editor's own keys. The text is a copy, in a private file under the session
directory (`.history-PANEID/`, removed when the editor ends): editing it
changes nothing in the pane, and opening it resizes nothing (the pane copied
keeps its size; its tile clips it while the editor is open; a second viewer
of the workspace, seeing the layout change, does fit it). The pane is
found by its id, and its screen is read once, when the key is pressed,
before its history: no line is missing between them, and a line that
scrolled off between the two reads can appear twice. When the editor ends,
the pane ends itself and the viewer that opened it closes its tile; if that
viewer was detached meanwhile, the tile stays, marked gone, until you close
it (`C-b x`). `history` is a command in the library: define your own in
`view.grift` to change any of this.

### Configuration: grift

Keys and commands are grift (grift 7, as fux embeds it). Rust gives the
primitives below; the commands, lists, prompts and menus are a library shipped
with the viewer (`src/library.grift`, then the keys in `src/defaults.grift`),
which `$XDG_CONFIG_HOME/fux3/view.grift` (else `~/.config/fux3/view.grift`)
extends or replaces: it is loaded last. Each part loads within 2 s and each
binding or list callback runs within 5 s; past that it is interrupted (and
forced a second later). Grift runs on a thread of its own, so a binding that
loops freezes nothing, and detach (`C-b d`, a verb) works while it runs.

| Primitive | |
|---|---|
| `(bind-key KEYS CAPTION HEADING FUNCTION)` | KEYS is up to 4 keys (`"C-b v"`, `"F7"`) |
| `(bind-verb KEYS CAPTION HEADING VERB [ARG])` | a viewer action, done at once even while a binding runs |
| `(bind-group KEYS CAPTION HEADING)`, `(unbind-key KEYS)` | a group's menu |
| `(act VERB [ARG])` | verbs: `next-pane`, `last-pane`, `focus-left` … `focus-down`, `next-tab`, `previous-tab`, `next-workspace`, `previous-workspace`, `show-tab "tN"`, `show-workspace "NAME"` (made if missing), `fit`, `detach`, `chord "C-b v"` |
| `(focused)`, `(shown-tab)`, `(shown-workspace)` | `NAME@ID` (`NAME@` for a bare tile no pane has), `tN`, the workspace's name |
| `(workspaces)` | the session's workspaces, by name |
| `(tabs)`, `(panes)`, `(key-bindings)` | lists of rows, of the workspace shown: `(tN title)`, `(NAME@ID tN)` (`NAME@` as for `focused`), `(keys caption heading)` |
| `(arrange LINE)` | one layout command; its result lines |
| `(fit-after RESULT)`, `(line-of KEY RESULT)` | fit the panes a result changed; a result line's value |
| `(start-new PREFIX PROGRAM ARG…)` | starts `PREFIX-N` in the workspace shown, or in WORKSPACE (made if missing) for `WORKSPACE/PREFIX` (your shell if no program): `NAME@ID` |
| `(close-workspace NAME)` | ends its panes, then removes its directory (refused, saying which, if a pane will not end within 12 s) |
| `(send PANE TEXT)` | to the pane's `in`, after keys typed before |
| `(ctl PANE LINE)` | one line to the pane's `ctl` (`kill`, `signal INT`, …). A pane that is gone (its directory went, or another life has its name) raises `pane-gone`, any other failure `fux3-error`: closing a pane or a tab ignores only the first, and keeps the tile otherwise |
| `(pick TITLE ROWS FUNCTION [KEYS])`, `(read-line PROMPT INITIAL FUNCTION)` | a list or a line to type; what is chosen goes to FUNCTION |
| `(focus PANE)`, `(say TEXT)`, `(workspace-dir)`, `(quote TEXT)` | `workspace-dir`: the shown workspace's directory, as it is called now |
| `(when-gone PANE FUNCTION)` | calls FUNCTION, once, when PANE (`NAME`, or one life `NAME@ID`) is gone from the workspace shown; at most 64 at a time |

```lisp
;; ~/.config/fux3/view.grift
(bind-key "F7" "run the tests" "Mine"
  (lambda! () (send (focused) "cargo test\r")))
(unbind-key "C-b n")
```

### Where fux3's viewer differs from fux

- **No server.** Panes are fux3 holders; the viewer reads their files, so a
  pane appears within a second of its directory, and a screen within about
  20 ms of its publish (fux3 publishes at most 25 times a second).
- **Ended panes keep their tiles.** A pane whose program ended, or whose
  directory is gone, keeps its tile, marked `NAME exited 3` or `NAME gone`,
  until you close it (`C-b x`); in fux an ended pane goes away.
- **Tab names.** The first tab is `all`, new ones `2`, `3`, …; fux names
  tabs by their first pane (`%1`).
- **Sizes stay.** A pane keeps its size when viewed, focused, or when the
  terminal is resized; only the split, move, border and zoom commands (and
  `C-b f`) fit the panes they change, once. fux resizes panes to their tiles.
- **No repeating groups.** `C-b r h` moves a border once; type it again to
  move it again. No `[cut]` or `[hidden]` marks in the corner, and no notice
  when a pane's program ends (its tile says so).
- **Pictures show only whole.** A picture is drawn (its file written as it
  is) only when all of it fits in its tile; one cut by the tile's edge, the
  bar or the top of what shows is left blank.
- **Keys typed while the keys load wait** in the terminal until the library,
  the default keys and your config have loaded (each part within its time).
- **Workspaces are directories.** One viewer shows one workspace; there is
  no key for a workspace rename (`C-b w r`), reorder (`C-b w m`), or moving a
  pane to another workspace (`C-b a w`, `C-b a W`): `mv` moves a pane (see
  "Moving and renaming a pane"). The list and next/previous go by name.
- **No swap.** `C-b a s` is gone; `move PANE DIR` swaps with a neighbour.
- **No border dragging with the mouse.** A click focuses; borders move with
  `C-b r h j k l` or `border PANE DIR CELLS`.
- **A pane's kill ends the panes started inside it.** On Linux a holder
  adopts what its program leaves behind, and `kill` ends it all: a viewer run
  in a pane of a session starts its new panes from inside that pane, so
  killing that pane ends them too. Run the viewer in your own terminal, or
  start panes with `fux3 start` from outside.
- **Scrollback and copying are your editor.** `C-b c` opens the pane's history
  and screen as one text in `$EDITOR` (else `$PAGER`, else `less +G`), in a
  pane of its own, which ends itself with the editor (see "Copying"). There
  is no copy mode.
- **The cursor's shape** is not in the pane files: the viewer shows the
  terminal's default.
- **Pane names, not titles.** There is no key to rename a pane (`C-b a r` is
  gone): `mv` renames one, and its tile follows. Tabs are renamed.
- **A new shell starts where the viewer runs**, not in the focused pane's
  directory, which the files do not say.
- **Layout filters are set when a tab is made** (`tab new … match KEY=VALUE`).
- **A pane killed outright** (its holder `kill -9`ed) shows as running until
  the next `fux3` command clears it, as `ls "$D"` would.
- **Running the viewer inside a pane of the session it views** shows itself;
  run it in your terminal, or in a pane of another session.

## What fux and fux2 do that fux3 doesn't

| fux / fux2 | fux3 |
|---|---|
| a server (fux) or a socket per pane (fux2) answering requests | files and two FIFOs; no request gets an answer, a client reads the effect |
| a guarded send (refused if the screen changed) | not yet: line 1's id and version leave room for one |
| screen patches and a blocking wait in the protocol | whole files; `fux3 wait` or `inotifywait` |
| a copy mode | history and screen as one text in your editor (`C-b c`) |
| mouse reporting to programs | the viewer takes the mouse for itself |
| respawn, dump and replay, crash takeover | none |
| Windows (fux) | Linux and macOS only |
| the raw output log | none: `history` is plain text (see above) |
