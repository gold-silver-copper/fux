# fux3's design

fux3 is a terminal multiplexer where a pane is a directory of ordinary files.
One small process, the **holder**, keeps each pane: it holds the program's
terminal (one pty, one `fux-vt` terminal), publishes what the terminal shows
as files, and takes typed bytes and commands from two FIFOs. There is no
server and no socket: any Unix tool is a client. The viewer, `fux3-view`, is
one more client of the same files.

## The session directory

`FUX3_DIR` if set (it must be absolute), else `$XDG_RUNTIME_DIR/fux3` on Linux
(a tmpfs: in memory) and `~/Library/Caches/fux3` on macOS (on disk; see the
README for putting it in memory). Made mode 0700 if missing; refused unless it
is a directory of this user's with no group or other bits.

| Entry | What |
|---|---|
| `NAME/` | a pane (`NAME` matches `[a-z0-9][a-z0-9_-]{0,31}`, so never a dot-name) |
| `.lock` | the session's lock: held briefly to claim a name or clear leftovers |
| `.view/` | the viewer's: the layout and its own `ctl` and `errors` (see fux3-view) |

## The pane directory

| Entry | Format and bound | Written by |
|---|---|---|
| `in` | FIFO, 0600. Raw bytes for the program. One write of at most 512 bytes (POSIX's least `PIPE_BUF`) is whole; longer writes may interleave with other writers: use `ctl`'s `send` | anyone of the user |
| `ctl` | FIFO, 0600. One command a line, each at most 512 bytes, so each arrives whole. Commands below | anyone of the user |
| `screen` | line 1 (below); then exactly ROWS lines, each a row's text with trailing blanks dropped | holder, atomic replace |
| `screen.styled` | the same line 1; then exactly ROWS lines, each the row's cells with the SGR changes between them, ending with the pen back at the default; then one line per picture shown (`picture N.sixel at=ROW,COL cells=CxR cropped=K cell=WxH`). Built only from the emulator's cells: the only control sequences in it are SGR | holder, atomic replace |
| `status` | one fact a line, `KEY VALUE` (below) | holder, atomic replace |
| `history` | plain text of the lines that scrolled off the main screen, appended; a soft-wrapped line is one line. At its cap (`--history`, default 4 MiB, at most 256 MiB) it is replaced by its newer half, starting at a line, so `tail -F` follows it | holder |
| `errors` | the last 32 refusals, a line each, `WHAT: WHY` (below) | holder, atomic replace |
| `images/N.sixel` | one picture shown, re-encoded as sixel from the emulator's pixels; written once, removed once no published screen shows it | holder |

Every file is 0600 and the directory and `images/` are 0700. Files replaced
whole are written to `.NAME.new` and renamed, so a reader sees an old file or
a new one, never part of one. The two screen files are renamed one after the
other (styled first), so a reader of both compares their line 1.

**Line 1** of `screen` and `screen.styled`:
`id=ID version=V size=COLSxROWS cursor=ROW,COL cursor-shown=yes|no`. `ID` is 16
hex digits, new at every start, so a new pane under an old name is told apart.
`V` advances on parsed output, an effective resize, the program's exit, a
`cell` change and a kill that left things behind. Rows and columns count from 0.
A reader skips fields it does not know.

**`status`**, one line each: `id`, `version` (of the screen this status was
published with), `name`, `size COLS ROWS`, `cell W H` (pixels), `pictures
on|off`, `state` (`running`, `exited CODE`, `signalled NAME`, or
`kill-incomplete WHAT`), `pid` (the program's, `none` once reaped), `holder`
(its own pid), `keys` (how the program asked for keys: `legacy cursor=…
keypad=… other-keys=… backarrow=…` or `kitty report=… alternate=…
backarrow=…`), `bracketed-paste`, `newline-mode`, `focus-reports`, `mouse MODE
ENCODING`, `alternate-screen`, `label KEY VALUE` per label, `commands N` (lines
`ctl` has taken, refused ones included), `refused N`, `queued N` (bytes waiting
for the program), and, when the last publish failed, `publish failed: WHY`.

## `ctl`'s commands

| Command | Effect seen in `status` | A refusal in `errors` |
|---|---|---|
| `resize COLS ROWS` | `size` changes, `version` advances | `N resize C R: WHY` (bad size, or the pty refused: nothing changed) |
| `send PATH` | `queued` grows by the file's size, then drains | `N send PATH: WHY` (not absolute, a link, not a regular file, not the user's, over 16 MiB, the queue full, the program ended): nothing was queued |
| `paste PATH` | as `send`, the file's UTF-8 text as one paste: bracketed if `bracketed-paste on`, its own end markers taken out | as `send`, or not UTF-8 |
| `cell W H` | `cell` changes; pictures drawn after it are laid out in it | `N cell W H: WHY` (smaller than 2x6, over 1000) |
| `kill` | the directory goes (complete) or `state kill-incomplete WHAT` (incomplete; send `kill` again) | `N kill: kill-incomplete WHAT` |
| anything else | `commands` and `refused` count it | `N LINE: no such command…` |

`N` is the command's number: `commands` after it was taken. **How a client
tells its own command's outcome:** read `commands` (say 6), write the command,
wait until `commands` is at least 7 (`fux3 wait` or a poll), then a line in
`errors` starting `7 ` is its refusal, and no such line means it was done.
With several writers at once the number is the holder's order, not the
writer's: a client that needs certainty is the only writer of `ctl`.

`send` and `paste` read the file whole before anything is queued: a regular
file of the same user, opened without following a link and without blocking
(so a FIFO or device is refused, not waited on), its size checked before it is
read and the read bounded. The bytes go into the input queue in one turn of
the holder's loop, so typing on `in` and other sends never land inside them.
`PATH` is the rest of the line, spaces included.

Typed bytes after the program ended are read and dropped, with a line
`in: N bytes dropped: the program has ended (STATE)`. The holder never removes
`in` while the directory stands (a shell's `> in` would make a regular file).

## The holder

**Start.** `fux3 start NAME [options] [-- PROGRAM ARGS]` starts `fux3 __hold
…` as a new session leader whose stdin is a pipe and stdout another, stderr
`/dev/null`. It writes `go`; the holder checks its arguments, adopts orphans
(Linux: it becomes the subreaper), claims the name, makes the FIFOs, the
history file and `images/`, starts the publisher thread, opens the pty, starts
the program, publishes its first screen, and only then writes `ok DIR` (or
`err WHY`, after removing what it made). `start` prints `DIR`, or the error
and exits 1; with no word in 10 s it says the pane may or may not have
started. The holder then points its stdin and stdout at `/dev/null`.

**The claim.** Under the session lock (`.lock`, flock, waited for at most
5 s): every pane directory whose lock can be taken is a dead holder's and is
removed (`remove_dir_all`, which removes links and never follows them, and
only if the directory is still the one that was locked); then `NAME/` is made
0700 and locked (flock on the directory itself, `O_NOFOLLOW|O_DIRECTORY`).
That lock, held for the holder's life, is the only proof a pane is alive.
Every `fux3` command clears dead panes this way. A plain `ls "$D"` can show a
directory whose holder was killed outright until the next `fux3` command; its
`status` then still says `running`.

**The loop.** One poll over: the SIGCHLD pipe, `ctl`, `in` (only while the
typed queue has room for a read; once the program ended, always, to drop), and
the pty (read while nothing waits to be parsed; write while bytes are queued).
Each turn: note the program's end; read at most 64 KiB of output; parse it 512
bytes at a time, each slice only when the reply queue has room for its
replies (so a program asking questions without end is slowed, never
answered partly); act on `ctl`'s lines in order; read `in`; write the queue to
the pty; take the rows that scrolled into history; publish if due.

Bounds: 64 KiB of output read at a time; the typed queue at most 16 MiB +
64 KiB (a whole `send` and typing beside it); replies at most 64 KiB (a reply
that does not fit is dropped and counted in `errors`); the terminal keeps 8 MiB
of history between looks (a look follows every read, so none is lost); 64
pictures and 16 MiB of pixels (`fux-vt` allows 96); `errors` 32 lines; screen
files at most 8 MiB (a larger one is not written, and `status` says so); at
most 128 panes a session.

**The program's end.** On SIGCHLD the holder reads the program's status
without reaping (so `kill` can still reach what it left), reads its last
output for 20 ms of quiet or at most 1 s, then sets `state exited CODE` or
`signalled NAME`, advances the version and keeps the last screen. Bytes still
queued are dropped with a line in `errors`.

**Kill.** Hang up the session (SIGHUP), give it 200 ms, SIGKILL the session; on
Linux also SIGKILL every descendant and adopted orphan (the holder is the
subreaper, so a `setsid` child is found); wait up to 2 s for the program and
up to 2 s for the rest, killing again what appears. If anything is left or a
process scan was incomplete, the directory stays, `state kill-incomplete WHAT`
says what, and `kill` may be sent again. Complete, the publisher writes what
waits, the directory is removed under the session lock, and the holder exits.
On macOS the holder is no subreaper: what left the program's session and
double-forked cannot be found, and is not reported.

## Publishing

Rendering happens on the loop's thread; writing on a publisher thread, so a
slow or full disk never stops the holder answering its program or reading
`ctl`. The loop hands over the newest screens (replacing ones not yet
written), history text (kept in order; past 4 MiB waiting, the oldest goes and
`history` says how much), and pictures to write or remove. A failed write
leaves the last good file in place and puts `publish failed: WHY` in the next
`status`.

**The timer.** A change marks the pane dirty. A dirty pane publishes at once if
the last publish was at least 40 ms ago, else when 40 ms have passed since it.
So the first change after a rest is published at once, output that goes on is
published at most 25 times a second, and the last change before a rest is
published within 40 ms.

**Pictures.** Each publish lists the pictures `fux-vt` shows. One not yet
written gets the next number and is handed to the publisher, which writes
`images/N.sixel` before the screens naming it; one no longer shown has its
file removed after the screens. New pictures are written at most 4 Mi pixels
a second on average (16 Mi at once); one over that waits for a later publish,
unlisted until written. So a program drawing without end costs a bounded
number of bytes written, and no picture file outlives its pane.

## `fux3 wait`

`fux3 wait NAME [--ms N]` reads line 1 of `screen` every 10 ms until its id or
version differs from the first look, then prints the new line 1; after `N` ms
(default 10000) it exits 1. On Linux `inotifywait -e moved_to "$D/NAME"` does
the same with no fux3.
