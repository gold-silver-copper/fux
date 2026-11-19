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
fux3 start build -- sh                      # prints the pane's directory, $D/build
printf 'echo hi\n' > "$D/build/in"          # type into it
cat "$D/build/screen"                       # line 1: the pane's id and version; then the screen
cat "$D/build/screen.styled"                # the same screen in colour
less "$D/build/history"                     # what scrolled off, as plain text
cat "$D/build/status"                       # size, state and exit code, pids, modes, labels
echo "resize 100 30" > "$D/build/ctl"       # a command; a refusal appears in errors
echo "send $HOME/paste.txt" > "$D/build/ctl" # a send of any size up to 16 MiB, whole or refused
ls "$D"                                     # the panes
fux3 view                                   # the panes as fux shows them (see "The viewer")
echo "split build right" > "$D/.view/ctl"   # the layout, driven with a plain file too
echo kill > "$D/build/ctl"                  # ends the program and all it started; the directory goes
```

## The commands

| Command | What |
|---|---|
| `fux3 dir` | prints the session directory, making it if missing |
| `fux3 start NAME [OPTIONS] [-- PROGRAM ARGS...]` | starts a pane; prints its directory. The program defaults to `$SHELL`. Options: `--size COLSxROWS` (default: the terminal it is run in, else 80x24; at most 500x200), `--cell WxH` (a cell's size in pixels for pictures; default: the terminal's, else 10x20), `--no-pictures`, `--history BYTES` (default 4 MiB, at most 256 MiB), `--label KEY=VALUE` (up to 16) |
| `fux3 ls` | lists the live panes |
| `fux3 wait NAME [--ms N]` | waits until NAME's screen changes (default 10000 ms), prints its new line 1; exit 1 if nothing changed in time |
| `fux3 view [--do LINE]` | the viewer; with `--do`, one layout command applied with no viewer running |

Every `fux3` command first clears the leftovers of panes whose holder is gone.
Names are 1 to 32 of `a-z 0-9 _ -`, starting with a letter or digit.

The session directory is `$FUX3_DIR` if set (an absolute path), else
`$XDG_RUNTIME_DIR/fux3` on Linux and `~/Library/Caches/fux3` on macOS. It and
everything in it are private to you (0700 and 0600); fux3 refuses a session
directory anyone else can open.

## The pane directory

| Entry | What it is |
|---|---|
| `in` | FIFO: bytes for the program, as typed. One write of at most 512 bytes arrives whole; for more use `send` |
| `ctl` | FIFO: commands, one line each (below) |
| `screen` | line 1, then the screen as plain text, a line per row |
| `screen.styled` | the same line 1, then the screen with colours and attributes, then one `picture …` line per picture |
| `status` | one fact a line: `KEY VALUE` |
| `history` | plain text of the lines that scrolled off the screen, appended; at its cap, cut to its newer half |
| `errors` | the last 32 refusals, a line each |
| `images/N.sixel` | one picture the program drew, as sixel: `cat` shows it in a sixel terminal |

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
`picture 1.sixel at=ROW,COL cells=COLSxROWS cropped=K cell=WxH` (ROW may be
negative: the picture's top has scrolled off; K top rows of it are gone).

**`status`**:

```
id 3f9a04c2b18e7d65
version 42
name build
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

`state` is `running`, `exited CODE`, `signalled NAME`, or `kill-incomplete
WHAT`. `pid` is the program's (`none` once it is gone), `holder` the holder's.
`keys` and the mode lines say how the program asked for its input. `commands`
counts the lines `ctl` has taken, refused ones included; `refused` those
refused; `queued` the bytes waiting for the program. When writing the files
failed (a full disk), a last line says `publish failed: WHY`; the last good
files stay.

**`history`** is plain text, never the program's raw bytes: raw bytes shown in
a terminal replay escape codes, make that terminal answer the program's
queries into your shell, and let a pane's program attack whoever tails it. A
soft-wrapped line is one line. `less +G` or `tail -F` reads it. It is appended to, not replaced, so a read while output pours in may end in the middle of a line; the next read has the rest.

## `ctl`

| Line | Effect, as `status` shows it | Refused: a line in `errors` |
|---|---|---|
| `resize COLS ROWS` | `size COLS ROWS`, a new `version` | bad numbers, or the terminal refused: nothing changed |
| `send PATH` | the file's bytes, whole, go to the program as typed; `queued` grows, then drains | not an absolute path, a link, not a regular file, not yours, over 16 MiB, the queue full, the program ended: nothing was sent |
| `paste PATH` | the file's text as one paste, as a terminal pastes: line breaks become CR, and it is bracketed if the program asked for that; otherwise as `send` | as `send`, or not UTF-8 |
| `signal NAME` | the signal (`INT`, `TERM`, `HUP`, …) goes to the program's foreground process group | no such signal, the program ended |
| `cell W H` | `cell W H`; pictures drawn after it use it | smaller than 2x6 or over 1000 |
| `kill` | the program and everything it started end, and the directory goes | `kill-incomplete WHAT` in `state` and in `errors`: send `kill` again |

A refusal reads `N LINE: WHY`, where `N` is the command's number, the value
of `commands` once it was taken. **Telling your command's outcome:** read
`commands` (say 6), write your line, wait until `commands` is 7 or more, then
look for a line starting `7 ` in `errors`: there is one only if it was
refused. With several writers of `ctl` at once, `N` is the holder's order.

`send` reads the whole file before anything is sent, so typing on `in` and
other sends never land inside it. The path is the rest of the line, spaces
and all.

What is typed into `in` is never read as a command: pasted text or a
program's echo cannot resize or kill a pane.

## Waiting for a change

`fux3 wait build` returns when `screen` changes. On Linux,
`inotifywait -e moved_to "$D/build"` does the same with no fux3. Or poll line 1.

The screens are published at once on the first change after a rest, at most
every 40 ms while output goes on, and once more within 40 ms when it rests.
Measured on Linux (an Apple M2 Max laptop, Arch Linux, tmpfs): a key written to `in` shows in
`screen` 2.2 ms later (median, 3.8 ms worst of 30); a pane printing without
end (`yes`) costs about 100 file writes a second (25 publishes of `screen`,
`screen.styled`, `status` and a `history` append), whatever the program does.

## When the program ends

`status` says `state exited CODE` (or `signalled NAME`) and the last screen
stays readable until you `kill` the pane. Bytes written to `in` afterwards
are read and dropped, each write noted in `errors`
(`in: 8 bytes dropped: the program has ended (exited 3)`); `in` itself stays,
so a shell's `> in` never makes a regular file there.

## Kill

`echo kill > "$D/NAME/ctl"` hangs up the program, then kills its session; on
Linux the holder is the subreaper of everything the program started, so a
`setsid` child or a double-forked daemon is found and killed too. When
everything is gone the directory is removed and the holder exits. When
something could not be killed, the directory stays, `state` says
`kill-incomplete` and what is left, and you may send `kill` again. On macOS a
process that left the program's session and was re-parented away cannot be
found, and is not reported.

## Two hazards of plain files

**A holder killed outright** (`kill -9`, a crash) leaves its directory until
the next `fux3` command clears it. Until then `ls "$D"` shows it, its `status`
still says `running`, and **`printf … > in` blocks for ever**: a FIFO with no
reader blocks its writer in `open`. `fux3 ls` tells live panes from dead ones
(and clears the dead). Guard a write you can't afford to hang:

```sh
timeout 2 sh -c 'printf "echo hi\n" > "$1"' sh "$D/build/in"        # Linux
perl -e 'alarm 2; exec @ARGV' sh -c 'printf "echo hi\n" > "$1"' sh "$D/build/in"   # macOS
```

**A write over 512 bytes to `in`** may arrive in pieces, mixed with another
writer's. Use `send` for anything long.

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

At most 128 panes a session, 500x200 cells a pane, 16 MiB a `send`, 16 MiB
waiting for a program, 512 bytes a `ctl` line, 64 KiB of the terminal's
replies waiting (more are dropped, noted in `errors`), 32 lines of `errors`,
8 MiB a screen file, 64 pictures and 16 MiB of their pixels. Everything past
a limit is refused out loud, in `errors`, on stderr or in `status`.

## What fux and fux2 do that fux3 doesn't

| fux / fux2 | fux3 |
|---|---|
| a server (fux) or a socket per pane (fux2) answering requests | files and two FIFOs; no request gets an answer, a client reads the effect |
| a guarded send (refused if the screen changed) | not yet: line 1's id and version leave room for one |
| screen patches and a blocking wait in the protocol | whole files; `fux3 wait` or `inotifywait` |
| a copy mode | `history` in a pager |
| mouse reporting to programs | the viewer takes the mouse for itself |
| respawn, dump and replay, crash takeover | none |
| Windows (fux) | Linux and macOS only |
| the raw output log | none: `history` is plain text (see above) |
