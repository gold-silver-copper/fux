# fux3's design

fux3 is a terminal multiplexer where a pane is a directory of ordinary files.
One small process, the **holder**, keeps each pane: it holds the program's
terminal (one pty, one `fux-vt` terminal), publishes what the terminal shows
as files, and takes typed bytes and commands from two FIFOs. There is no
server and no socket: any Unix tool is a client. The viewer, `fux3-view`, is
one more client of the same files.

## The rule

A pane is its directory and its id; a workspace is its directory; a name is
only where one is listed right now. Whoever acts on one opens it once and
stays on that object. A thing gets its name by one rename once it is whole
and loses it by one rename before it is taken apart. A layout command is an
edit of the layout file, not a message to a process.

What follows in the code:

- **Handles, not paths.** The session, a workspace and a pane are each a
  directory held open (`session.rs`), and every file is named relative to
  one (`fuxix::file::at`: `openat`, `mkdirat`, `mkfifoat`, `renameat`,
  `unlinkat`, `fdopendir`). No path is looked up twice. The holder writes,
  reads its FIFOs and takes itself apart through its pane's handle, so `mv`
  of a live pane changes nothing for it. A path is kept only to show, and
  to hand to another program (the recipe's `cd`, the pane's variables).
- **Where a directory is listed now** is asked only when its name must
  change (`session::place`): open `..` through the handle, find the entry
  with its inode, check it is the same directory. Not found is "removed".
- **Naming is one rename that replaces nothing** (`renameat2` with
  `RENAME_NOREPLACE` on Linux, `renameatx_np` with `RENAME_EXCL` on macOS),
  under the session's lock. A plain `mv` takes no lock, so the name a
  directory was found under may hold another by the time it is renamed:
  after every such rename, what has the new name is checked to be the
  directory held, and given its name back if it is not.
- **A command that names a workspace holds its directory from then on.**
  `Workspace::check` says "workspace W was removed" once that directory is
  no longer listed in the session, whatever has its name; `fux3 start
  --workspace-id` lets a caller that named the workspace earlier (the
  viewer) start a pane in that directory or not at all (such a start makes
  no workspace).

## The session directory

`FUX3_DIR` if set (it must be absolute), else `$XDG_RUNTIME_DIR/fux3` on Linux
(a tmpfs: in memory) and `~/Library/Caches/fux3` on macOS (on disk; see the
README for putting it in memory). Made mode 0700 if missing; refused unless it
is a directory of this user's with no group or other bits.

| Entry | What |
|---|---|
| `WORKSPACE/` | a workspace: a directory of the user's (fux3 makes it 0700; `mkdir` makes one too), holding its panes; nothing of its own but `.view/` and `.recipes/` |
| `WORKSPACE/NAME/` | a pane (both names match `[a-z0-9][a-z0-9_-]{0,31}`, so never a dot-name) |
| `WORKSPACE/.new-ID/` | a pane being made: whole but for its name |
| `WORKSPACE/.gone-ID/` | a pane taken out of its name, being taken apart |
| `WORKSPACE/.view/` | the viewer's: the workspace's layout and its lock (see the README) |
| `WORKSPACE/.recipes/` | a recipe per pane name |
| `.lock` | the session's lock: held briefly to give or take a name, and to clear dead panes |

A workspace is made (0700) when its first pane starts, or by `mkdir`; an empty
one is left alone. Only the session directory must be private (0700, the
user's): everything in it is behind it. Every `fux3` command clears dead
panes in every workspace. The pane limit, 128, is per workspace and checked
when a pane is named.

## The pane directory (`WORKSPACE/NAME/`)

| Entry | Format and bound | Written by |
|---|---|---|
| `in` | FIFO, 0600. Raw bytes for the program. One write of at most 512 bytes (POSIX's least `PIPE_BUF`) is whole; longer writes may interleave with other writers: use `ctl`'s `send` | anyone of the user |
| `ctl` | FIFO, 0600. One command a line, each at most 512 bytes, so each arrives whole. Commands below | anyone of the user |
| `screen` | line 1 (below); then exactly ROWS lines, each a row's text with trailing blanks dropped | holder, atomic replace |
| `screen.styled` | the same line 1; then exactly ROWS lines, each the row's cells with the SGR changes between them, ending with the pen back at the default; then one line per picture shown (`picture N.sixel at=ROW,COL cells=CxR cropped=K cell=WxH`). Built only from the emulator's cells: the only control sequences in it are SGR | holder, atomic replace |
| `status` | one fact a line, `KEY VALUE` (below) | holder, atomic replace |
| `history` | plain text of the lines that scrolled off the main screen, appended as whole **records**; a soft-wrapped line is one line. A record is a line with its newline, at most min(cap/2, 64 KiB) bytes; a longer line is pieces, each a record ending ` [fux3: continued]` before its newline (taking the mark and that newline out gives the line back). The holder gathers a line until it ends or is a piece long, so nothing but whole records reaches the file. Each file holds at most half the cap (`--history`, default 4 MiB, at most 256 MiB): a record that does not fit starts a new one, `history` renamed `history.1` (replacing it), so rotation falls only between records and `tail -F` follows it and repeats nothing | holder |
| `history.1` | the generation before | holder, by rename |
| `errors` | the last 32 refusals, a line each, `WHAT: WHY` (below) | holder, atomic replace |
| `images/N.sixel` | one picture shown, re-encoded as sixel from the emulator's pixels; written once, removed once no published screen shows it | holder |
| `exit` | `state exited CODE` or `state signalled NAME` and a newline, once the program's end has settled; written aside and renamed, after the screens that show the end, only when they and every picture they name were written whole (a picture not written is tried again with each publish); tried again each second until it is on disk; a failed write of it is in `status` | holder |
| `id` | the pane's id and a newline: the marker that makes a directory a pane's. Written first, before the directory has its name | holder, once |

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
published with), `size COLS ROWS`, `cell W H` (pixels), `pictures
on|off`, `state` (`starting` until the program is started, `running`,
`exited CODE`, `signalled NAME`, or `kill-incomplete WHAT`), `pid` (the program's, `none` once reaped), `holder`
(its own pid), `keys` (how the program asked for keys: `legacy cursor=…
keypad=… other-keys=… backarrow=…` or `kitty report=… alternate=…
backarrow=…`), `bracketed-paste`, `newline-mode`, `focus-reports`, `mouse MODE
ENCODING`, `alternate-screen`, `label KEY VALUE` per label, `commands N` (lines
`ctl` has taken, refused ones included), `refused N`, `queued N` (bytes waiting
for the program), and, when the last publish failed, `publish failed: WHY`.
It names no workspace and no pane: a name is where the directory is listed,
which the reader came by.

## `ctl`'s commands

| Command | Effect seen in `status` | A refusal in `errors` |
|---|---|---|
| `resize COLS ROWS` | `size` changes, `version` advances | `N resize C R: WHY` (bad size, or the pty refused: nothing changed) |
| `send PATH` | `queued` grows by the file's size, then drains | `N send PATH: WHY` (not absolute, a link, not a regular file, not the user's, over 16 MiB, the queue full, the program ended): nothing was queued |
| `paste PATH` | as `send`, the file's UTF-8 text as one paste: bracketed if `bracketed-paste on`, its own end markers taken out | as `send`, or not UTF-8 |
| `cell W H` | `cell` changes; pictures drawn after it are laid out in it | `N cell W H: WHY` (smaller than 2x6, over 1000) |
| `signal NAME` | nothing (the program decides); any of fuxix's names (`INT`, `TERM`, `HUP`, `QUIT`, `KILL`, `TSTP`, `CONT`, `USR1`, …); a leading `SIG` is dropped. Sent to the terminal's foreground process group | `N signal NAME: WHY` (no such signal, the program ended) |
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

Typed bytes after the program ended, or once it closed its terminal, are read
and dropped, counted in one line of `errors` replaced each time
(`in: N bytes dropped in all: WHY`), so they never push a command's refusal out.
`send` and `paste` are refused then too. The holder never removes
`in` while the directory stands (a shell's `> in` would make a regular file).

## The holder

**Start.** `fux3 start NAME [options] [-- PROGRAM ARGS]` starts `fux3 __hold`
with its own terminal's size and cell and then its own arguments, as a new
session leader whose stdin is a pipe and stdout another, stderr `/dev/null`.
The holder reads those arguments itself: there is one reading of them. The
values of `--env` are not among them (arguments are public): they come on
the pipe after `go`, each ended by a NUL. The holder checks everything,
adopts orphans (Linux: it becomes the subreaper), and then:

1. **Stages.** Under the session's lock it makes `WORKSPACE/.new-ID/` (0700),
   opens it and locks it (flock on the directory itself, held for the
   holder's life: the only proof a pane is alive), and writes `id`. Then,
   with no lock: the FIFOs, `images/`, `history`, the publisher thread, the
   pty; and the first `screen`, `screen.styled` and `status` (`state
   starting`), waited for until they are on disk.
2. **Names.** Under the session's lock: dead panes are cleared, a live
   pane's name or a full workspace is refused, and one rename that replaces
   nothing gives the directory its name. A reader that finds the name finds
   a whole pane. Anything else at the name (a directory no pane made) is
   refused and left alone.
3. **Starts the program**, publishes (`state running`), waits for that to
   be on disk, writes the recipe, and only then writes `ok` (or `err WHY`,
   after its own `kill`). So a second start of a live name never runs its
   program, and a start that fails leaves the last good recipe.

`start` prints the directory, or the error and exits 1; with no word in 10 s
it says the pane may or may not have started. The holder then points its
stdin and stdout at `/dev/null`.

**Clearing dead panes.** Under the session's lock, in every workspace, every
directory named as a pane, or `.new-*`, or `.gone-*`, whose lock can be
taken is dead: a holder killed outright left it, at whichever step. One with
a pane's name first loses it, renamed `.gone-ID` (`.gone-ID-N` where that
is taken, by whatever: the rename that replaces nothing says so) as in its
own kill, so no
reader finds a name half empty and a clearing that dies leaves what the next
one takes. Then it is
cleared through its handle (`clear_pane`): only with the marker (`id`
holding 16 hex digits and a newline, as a holder writes it: a file that is
only called `id` makes no directory a pane's; or, a
`.new-*`/`.gone-*` with nothing in it), and only the files a holder makes,
by name (`in`, `ctl`, the screens, `status`, `history`, `history.1`,
`errors`, `exit`, the `.NAME.new` of those written whole (the screens,
`status`, `errors`, `exit`, `.images-N.sixel.new`), `images/N.sixel`, and
last `id`). No
directory but `images/` is entered, so a pane moved into another's
directory, its lock held or not, is never touched. With anything else in it,
that and the marker stay, in `.gone-ID` (a later clearing tries again);
emptied, it is removed, if the name still holds the directory that was
locked. Every `fux3` command but `put` clears this way. A
plain `ls "$D"` can show a directory whose holder was killed outright until
the next `fux3` command; its `status` then still says `running`.

The marker can be forged and a `cp -a` of a pane copies it: a copy inside a
workspace is a dead pane and is cleared as one. Same user, same trust.

**The loop.** One poll over: the SIGCHLD pipe, `ctl`, `in` (only while the
typed queue has room for a read; once the program ended, always, to drop), and
the pty (read while nothing waits to be parsed; write while bytes are queued).
Each turn: note the program's end; read at most 16 KiB of output; parse it 512
bytes at a time, each slice only when the reply queue has room for its
replies (so a program asking questions without end is slowed, never
answered partly); act on `ctl`'s lines in order; read `in`; write the queue to
the pty; take the rows that scrolled into history; publish if due.

Bounds: 16 KiB of output read at a time; the typed queue at most 16 MiB +
64 KiB (a whole `send` and typing beside it); replies at most 64 KiB (a reply
that does not fit is dropped and counted in `errors`); the terminal keeps 2 MiB
of history between looks (a look follows every read, enough for ordinary
text; rows lost to output built to fill rows cheaply are counted in
`history`; kept small because a resize reflows all of it); 64
pictures and 16 MiB of pixels (`fux-vt` allows 96); `errors` 32 lines; screen
files at most 8 MiB (a larger one is not written, and `status` says so); at
most 128 panes a session.

**The program's end.** On SIGCHLD the holder reads the program's status
without reaping (so `kill` can still reach what it left), reads its last
output for 20 ms of quiet or at most 1 s, then sets `state exited CODE` or
`signalled NAME`, advances the version and keeps the last screen. Bytes still
queued are dropped with a line in `errors`.

**Kill**, by `kill` on `ctl` or SIGTERM to the holder. Hang up the session
(SIGHUP), give it 200 ms; on Linux ask everything else the program started to
end (SIGTERM to every descendant and adopted orphan: a holder among them, of
a pane started inside this one, runs its own kill and takes its pane apart)
and give that up to 1 s; SIGKILL the session; on Linux also SIGKILL every
descendant and adopted orphan left (the holder is the subreaper, so a
`setsid` child is found); wait up to 2 s for the program and up to 2 s for
the rest, killing again what appears. If anything is left or a process scan
was incomplete, the directory stays, `state kill-incomplete WHAT` says what,
and `kill` may be sent again. Complete:

1. under the session's lock, only this: the directory is renamed
   `.gone-ID` where it is listed now (wherever `mv` put it). The name is
   free at once, and nothing waits on a disk under the lock;
2. the publisher writes what waits and stops;
3. the directory is cleared through its handle, as a dead pane's is, and
   removed if that emptied it;
4. if the kill ended anything outside the program's session (found at the
   SIGTERM or the SIGKILL scan), dead panes are cleared once more, for what it ended outright (an inner
   holder that did not end itself); and the holder exits.

A pane listed nowhere, or whose listing cannot be asked (its parent
unreadable), is left as it is for a clearing that finds it.

A holder that dies between 1 and 3 leaves `.gone-ID`, which the next
clearing takes. A rename refused leaves the pane whole, to say so. On macOS
the holder is no subreaper: what left the program's session and
double-forked, an inner pane's holder among it, cannot be found, and is not
reported.

## Publishing

Rendering happens on the loop's thread; writing on a publisher thread, so a
slow or full disk never stops the holder answering its program or reading
`ctl`. Each hand-over is written history first, then pictures, the
screens, `errors`, `status` and `exit`: a line that scrolled off is in
`history` before a screen without it is, so a reader of `screen` and then
`history` may see a line twice and never misses one. The loop hands over the newest screens (replacing ones not yet
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

`fux3 wait [WORKSPACE/]NAME [--ms N]` reads line 1 of `screen` every 10 ms until its id or
version differs from the first look, then prints the new line 1; after `N` ms
(default 10000) it exits 1. On Linux `inotifywait -e moved_to "$D/WORKSPACE/NAME"` does
the same with no fux3.

## Recipes and `fux3 put`

The holder writes `WORKSPACE/.recipes/NAME`, in the workspace directory it
holds, last: once the pane has its name, its program runs and the files say
so, before it says `ok` (a recipe it can't write fails the start; a start
that fails writes none, so the last good one stays; and no older start can
overwrite a newer one's): an
executable `sh` script (`#!/bin/sh`, `# fux3 size COLSxROWS`, one `# fux3
label KEY=VALUE` per label, `cd -- 'DIR' || exit 126`, one
`NAME='VALUE'; export NAME` per `--env`, `exec 'PROGRAM' 'ARG'…`, or `exec env -- 'PROGRAM' …` for a program
whose name starts with `-`), every word
single-quoted with `'` written `'\''`, so any byte but NUL comes back as it
was. Rendered before the start, refused past 64 KiB; written aside and
renamed, 0700, in a 0700 `.recipes/`. Only the variables named by `--env` go
in it, never the inherited environment. A name `sh` keeps for itself (`PPID`,
`UID`, `EUID`, `BASHOPTS`, `SHELLOPTS`, `BASH_VERSINFO`) is refused at start.
With no program, `start` runs `/bin/sh .recipes/NAME` when that file is
there, with the size and labels its comment lines keep unless the start
gives its own, and writes no new recipe. A recipe is found by name: it does
not follow a pane that `mv` renamed.

With no program, `start` runs a recipe only if it is a plain file of at most
64 KiB; anything else at the name is refused.

`fux3 put` takes no lock and clears nothing. It opens the workspace and then
the pane's directory, once, and asks everything of that handle: the marker
(else "no pane"), the lock (taken: a dead holder; an error asking: a
refusal, never success), `status` (it must be readable and say `state
running`: a write to `in` only queues, and the holder drops what no program
will read, so an ended program, a `kill-incomplete` and a `status` that
cannot be read are all refused), then `in`, opened relative to the handle with
`O_WRONLY|O_NONBLOCK|O_NOFOLLOW` and no create: no file is "no pane",
`ENXIO` (nobody reads) is "a dead holder". So a new pane that took the name
after the lookup is never typed into. It writes at most `PIPE_BUF` (512)
bytes in one call, so the write is whole or `EAGAIN` ("input full"). It
never waits.

## Left open

- **A holder killed outright from outside** (`kill -9`, out of memory) stays
  documented (owner, 2026-11-19): its directory stays until the next `fux3`
  command, and a plain write to its `in` or `ctl` can block. The panel was
  split between request files in place of the FIFOs (5), a keeper process
  per pane (3) and documenting it (2). The README gives the safe ways.
- **After `mv`**: the recipe stays under the old name; `FUX3_WORKSPACE`,
  `FUX3_PANE` and `FUX3_PANE_DIR` are true only at start; a name outside the
  rule and the pane limit are not checked; `mv` onto a pane nests; `mv` to
  another filesystem copies and deletes. Each is in the README.
- **The kill lock**: a filesystem that stalls inside the one rename still
  holds the session's lock for that long; one that stalls a pending write
  keeps the holder (its name already free) from exiting.
- **Windows of a `mv` nobody takes a lock for**: an empty directory moved to
  a name in the instant between its check and its `rmdir` is removed; a
  recipe is run by `sh` from its path, so a workspace renamed and another
  made under its name between a start's naming and its `sh` runs the other's
  recipe. Review round 1 named both; neither loses a file.
- **An inner holder killed outright** whose pane is in another session, or
  under another pane's directory, is not cleared by the outer kill.
- **`put` and a program whose `exit` cannot be written** (a full disk):
  `put` writes, and the holder drops it with a line in `errors`.
- **macOS**: an inner pane's holder is not found by the outer kill.

## Ideas not built

From the analogy panels of 2026-11-19 (`notes/projects/fux/fux3-2026-11-18/
analogies-result.md`), for the owner to pick up later:

- **An `events` file per workspace.** Lossless only with a lock around append
  and rotation (36,000 of 36,000 with it; lines lost without).
- **A `notices` file per workspace.** Appending is safe; rotation without a lock
  lost lines, so it needs a command or a holder to rotate.
- **A watch-only export** outside the private directories: group permissions
  worked, but a replace reset the mode and `history`'s size leaked.
- **A `cwd` link; aliases; `in` and `ctl` as a spool of files; a whole-pane
  capture as a tar.** Spiked or proposed, not needed by the example.

## Tried and rejected

- A per-pane `events` file: `tail -F` never sees a pane made after it starts;
  50 of 100 records lost across two rotations.
- Labels replaced by folders: two labels cut across each other, no folder holds
  both.
- Waiting by a blocking FIFO read: a change between the check and the open was
  missed 50 of 50 times.
- Delete-to-kill: after `rm -r` the holder and program lived on with nothing left
  to kill them with.
- Tabs as folders of symlinks: a reader saw one pane in two tabs 2,121 times in
  3,000 looks during a correct move.
- Child panes as subfolders: nothing the kill rule doesn't give, and it broke a
  pane started from outside.
- A `.view/focused` link: a writer through it wedged in 15 of 15 kills.
- A `tty` link: `stty` through it resized the pty behind the holder's back.
- `tee` into many panes: one stopped pane stalls them all; a 64 KiB write split.
- Activity from file times: `find -newer` flagged 48 of 48 repaints that changed
  nothing visible.
