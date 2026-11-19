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
echo "split build right" > "$D/main/.view/ctl" # the layout, driven with a plain file too
echo kill > "$D/main/build/ctl"             # ends the program and all it started; the directory goes
```

## The commands

| Command | What |
|---|---|
| `fux3 dir` | prints the session directory, making it if missing |
| `fux3 start [WORKSPACE/]NAME [OPTIONS] [-- PROGRAM ARGS...]` | starts a pane in WORKSPACE (default `main`, made if missing); prints its directory. The program defaults to `$SHELL`. Options: `--size COLSxROWS` (default: the terminal it is run in, else 80x24; at most 500x200), `--cell WxH` (a cell's size in pixels for pictures; default: the terminal's, else 10x20), `--no-pictures`, `--history BYTES` (default 4 MiB, 4096 bytes to 256 MiB), `--label KEY=VALUE` (up to 16) |
| `fux3 ls` | lists the live panes, as `WORKSPACE/NAME` |
| `fux3 wait [WORKSPACE/]NAME [--ms N]` | waits until the pane's screen changes (default 10000 ms), prints its new line 1; exit 1 if nothing changed in time |
| `fux3 view [WORKSPACE] [--do LINE]` | the viewer of a workspace (below); with `--do`, one layout command applied with no viewer running |

Every `fux3` command first clears the leftovers of panes whose holder is gone,
in every workspace. Pane and workspace names are 1 to 32 of `a-z 0-9 _ -`,
starting with a letter or digit; a pane's name is unique in its workspace.

## Workspaces

A workspace is a directory of the session, and a pane a directory of its
workspace: `$D/rig/build/`. `ls "$D"` lists the workspaces, `ls "$D/rig"` a
workspace's panes. A workspace is made when its first pane starts (or by
`mkdir -m 700 "$D/rig"`) and has no file, process or state of its own but the
viewer's `.view/` (its layout). An empty workspace stays until you remove it
(`rmdir "$D/rig"`, after removing its `.view` if a viewer made one). A pane's
workspace is fixed when it starts.

Ending a workspace is ending each pane in it:

```sh
fux3 ls >/dev/null; for p in "$D/rig"/*/; do echo kill > "${p}ctl"; done
```

(`fux3 ls` first clears dead panes, whose `ctl` would block the writer.)

A program in a pane finds itself in its environment: `FUX3_DIR` (the
session), `FUX3_WORKSPACE`, `FUX3_PANE` (its name) and `FUX3_PANE_DIR` (its
directory).

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
| `history` | plain text of the lines that scrolled off the screen, appended |
| `history.1` | the history before `history` last reached half its cap (there once it has) |
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
`picture 1.sixel at=ROW,COL cells=COLSxROWS cropped=K cell=WxH`. ROW is
negative once the picture's top rows have scrolled into history; a picture
with no row left on the screen is no longer listed and its file goes. K
counts top rows cut off for good by a scroll region or the alternate screen.
Writing text over any cell of a picture removes the whole picture.

**`status`**:

```
id 3f9a04c2b18e7d65
version 42
workspace main
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
soft-wrapped line is one line. `less +G` or `tail -F` reads it. It is appended to, so a read while output pours in may end in the middle of a line; the next read has the rest. Each file holds at most half the cap (`--history`, default 4 MiB): past it, `history` is renamed `history.1` (the older `history.1` goes) and a new `history` starts. `tail -F history` follows the new file and sees every line once; `tail -f` stops at the old one. `cat history.1 history` is everything kept, in order, under the cap; what neither holds is gone.

## `ctl`

| Line | Effect, as `status` shows it | Refused: a line in `errors` |
|---|---|---|
| `resize COLS ROWS` | `size COLS ROWS`, a new `version` | bad numbers, or the terminal refused: nothing changed |
| `send PATH` | the file's bytes, whole, go to the program as typed; `queued` grows, then drains | not an absolute path, a link, not a regular file, not yours, over 16 MiB, the queue full, the program ended: nothing was sent |
| `paste PATH` | the file's text as one paste, as a terminal pastes: line breaks become CR, and it is bracketed if the program asked for that; otherwise as `send` | as `send`, or not UTF-8 |
| `signal NAME` | the signal (`INT`, `TERM`, `HUP`, …) goes to the program's foreground process group | no such signal, the program ended |
| `cell W H` | `cell W H`; pictures drawn after it use it | smaller than 2x6 or over 1000 |
| `kill` | the program and everything it started end, and the directory goes | `kill-incomplete WHAT` in `state` and in `errors`: send `kill` again |

Once the program has ended, every command but `kill` is refused. Blank lines
are ignored and not counted.

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
are read and dropped, each write noted in `errors`
(`in: 8 bytes dropped: the program has ended (exited 3)`); `in` itself stays,
so a shell's `> in` never makes a regular file there.

## Kill

`echo kill > "$D/WORKSPACE/NAME/ctl"` hangs up the program, then kills its session; on
Linux the holder is the subreaper of everything the program started, so a
`setsid` child or a double-forked daemon is found and killed too.

**Panes started inside a pane are part of it.** One kill ends a task and
everything it started, panes included: on Linux, a `fux3 start` run inside a
pane makes a holder that pane's holder adopts, so killing the outer pane ends
the inner one too. Its holder is killed outright, so its folder stays, with `status` still saying `running`, until the next `fux3` command clears it (as with any holder killed outright, below). A viewer
run inside a pane starts its shells inside that pane, so killing that pane
ends them. On macOS the inner pane's holder left the outer pane's session and
cannot be found, so it outlives the outer pane: end it with
`echo kill > "$D/WORKSPACE/NAME/ctl"`. When
everything is gone the directory is removed and the holder exits. When
something could not be killed, the directory stays, `state` says
`kill-incomplete` and what is left, and you may send `kill` again. On macOS a
process that left the program's session and was re-parented away cannot be
found, and is not reported.

## Two hazards of plain files

**A holder killed outright** (`kill -9`, a crash) leaves its directory until
the next `fux3` command clears it. Until then `ls "$D/main"` shows it, its `status`
still says `running`, and **`printf … > in` blocks for ever**: a FIFO with no
reader blocks its writer in `open`. `fux3 ls` tells live panes from dead ones
(and clears the dead). Guard a write you can't afford to hang:

```sh
timeout 2 sh -c 'printf "echo hi\n" > "$1"' sh "$D/main/build/in"        # Linux
perl -e 'alarm 2; exec @ARGV' sh -c 'printf "echo hi\n" > "$1"' sh "$D/main/build/in"   # macOS
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

At most 128 panes a workspace, 500x200 cells a pane, 16 MiB a `send`, 16 MiB
waiting for a program, 512 bytes a `ctl` line, 64 KiB of the terminal's
replies waiting (more are dropped, noted in `errors`), 32 lines of `errors`,
8 MiB a screen file, 64 pictures and 16 MiB of their pixels. Everything past
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
echo "split build right" > "$D/main/.view/ctl"  # every viewer of main changes
fux3 view main --do 'zoom build'                # the same with no viewer running
fux3 view rig                                   # workspace rig, made if missing
```

`fux3 view [WORKSPACE]` shows WORKSPACE (default `main`, made if missing, as
`fux3 start` does). The workspace keys go over the session's workspaces, the
directories of `$D`, by name: next, previous, a list, new (a name at a prompt:
the directory is made and a shell started in it) and close (asks, then ends
each pane, waits for them to go, and removes the directory, `.view` and all).
A viewer whose workspace's directory goes shows the first one left, else a new
`main`. Each viewer remembers, for each workspace it has shown, the tab and
focus it left there. Workspaces have no rename (a directory renamed under live
holders moves their files away from them) and no order but their names'; a
pane stays in the workspace it started in.

### What it reads and writes

Paths are in the workspace shown, `$D/WORKSPACE/`.

| Path | What the viewer does with it |
|---|---|
| `NAME/screen.styled` | drawn in the pane's tile; looked at every 20 ms while shown, and read again only when replaced (its inode, size or time changed) |
| `NAME/status` | the pane's id, labels, state, size, key mode and newline mode; looked at the same way |
| `NAME/images/N.sixel` | the pictures the screen names: one whole in its tile is written as it is, at its place, when this terminal's DA1 says it draws sixel; else its cells stay blank |
| `NAME/in` | your keys, encoded for the mode `status` gives (`keys`); Enter is CR LF under `newline-mode on`; at most 512 bytes a write, queued (64 KiB at most) while the pipe is full |
| `NAME/ctl` | `resize COLS ROWS` (only the fit step below), `paste PATH`, and whatever your grift sends (`kill`, `signal NAME`) |
| `NAME/history` | paged in a pane of its own by `C-b c` |
| `.view/layout`, `.view/layout.lock` | the workspace's layout, shared by every viewer of it (below) |
| `.view/ctl` | a FIFO of layout commands for this workspace, made by the first viewer of it; every viewer showing the workspace reads it, so any one takes a line, and a viewer stops reading it when it shows another or leaves; the last one to stop removes it. So with no viewer, `echo LINE > .view/ctl` makes a regular file, whose lines the next viewer of the workspace runs when it comes. A viewer killed outright leaves the FIFO, and a writer then waits for a reader (guard it as for `in`, below, or use `--do`) |
| `.view/errors` | the last 32 refused `.view/ctl` lines, `LINE: WHY`, replaced whole |
| `.view/paste-PID-N` | a paste's text, 0600, removed once the pane's `commands` count passed the `paste` command, or after 10 s |

The workspace's directory is listed again once a second and whenever the
layout changes. A pane whose directory is gone keeps its tile, drawn as `NAME gone`;
one whose program ended keeps it too, marked `NAME exited 0` (or `signalled
…`). A new pane under an old name gets its own tile: tiles name a pane with its
id. Panes the viewer starts get the first free `PREFIX-N` that no pane and no
tile uses.

### The layout file

`.view/layout` holds the workspace's tabs, and per tab a tree of splits with
relative shares over panes named `NAME@ID`, a zoom, and a filter:

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

One line each, acting on one workspace's layout: written to its `.view/ctl` (a
viewer of it takes it), run with `fux3 view [WORKSPACE] --do 'LINE'` (prints
what it did; a refusal goes to stderr, exit 1), or from grift with `(arrange
LINE)` (the workspace shown). `PANE` is `NAME` or `NAME@ID`, a pane of that
workspace; `TAB` is `tN` or a title no other tab has; `DIR` is `left`, `right`,
`up` or `down`; a title with spaces is quoted, `"like this"`.

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
every pane whose tile moved or changed size.

**Sizes.** Opening a viewer, moving the focus and resizing the viewer's
terminal resize no pane: a tile clips or pads it. The shipped commands end by fitting the
panes they changed, once (`resize COLS ROWS` on each one's `ctl`, the size of
its tile in the viewer that ran the command): that step is the library's
`edit!`; `.view/ctl` lines go through `layout-command`, which is `edit!`.
Define `layout-command` as `arrange` in your config to leave sizes alone. `C-b
f` fits the focused pane by hand. `--do` fits nothing.

### Keys

fux's keys, wherever fux3 has the command. `C-b` opens the prefix menu, drawn
from the bindings alone (each has a caption and a heading); a key bound to a
group opens that group's menu, and Escape closes it. `C-b` twice sends `C-b`.

| Keys | |
|---|---|
| `C-b v` / `C-b s` | split side by side / stacked (a new shell) |
| `C-b x`, `C-b z`, `C-b c`, `C-b f` | close pane (asks), zoom or restore, history in a pager, fit the pane to its tile |
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
| `(focused)`, `(shown-tab)`, `(shown-workspace)` | `NAME@ID`, `tN`, the workspace's name |
| `(workspaces)` | the session's workspaces, by name |
| `(tabs)`, `(panes)`, `(key-bindings)` | lists of rows, of the workspace shown: `(tN title)`, `(NAME@ID tN)`, `(keys caption heading)` |
| `(arrange LINE)` | one layout command; its result lines |
| `(fit-after RESULT)`, `(line-of KEY RESULT)` | fit the panes a result changed; a result line's value |
| `(start-new PREFIX PROGRAM ARG…)` | starts `PREFIX-N` in the workspace shown, or in WORKSPACE (made if missing) for `WORKSPACE/PREFIX` (your shell if no program): `NAME@ID` |
| `(close-workspace NAME)` | ends its panes, then removes its directory (refused, saying which, if a pane will not end within 12 s) |
| `(send PANE TEXT)` | to the pane's `in`, after keys typed before |
| `(ctl PANE LINE)` | one line to the pane's `ctl` (`kill`, `signal INT`, …) |
| `(pick TITLE ROWS FUNCTION [KEYS])`, `(read-line PROMPT INITIAL FUNCTION)` | a list or a line to type; what is chosen goes to FUNCTION |
| `(focus PANE)`, `(say TEXT)`, `(workspace-dir)`, `(quote TEXT)` | `workspace-dir`: the shown workspace's directory |

```lisp
;; ~/.config/fux3/view.grift
(bind-key "F7" "run the tests" "Mine"
  (lambda! () (send (focused) "cargo test\r")))
(unbind-key "C-b n")
(define! layout-command (lambda! (line) (arrange line)))   ; .view/ctl resizes nothing
```

### Where fux3's viewer differs from fux

- **No server.** Panes are fux3 holders; the viewer reads their files, so a
  pane appears within a second of its directory, and a screen within about
  20 ms of its publish (fux3 publishes at most 25 times a second).
- **Sizes stay.** A pane keeps its size when viewed, focused, or when the
  terminal is resized; only the split, move and border commands (and `C-b f`)
  fit it, once. fux resizes panes to their tiles.
- **No repeating groups.** `C-b r h` moves a border once; type it again to
  move it again. No `[cut]` or `[hidden]` marks in the corner, and no notice
  when a pane's program ends (its tile says so).
- **Pictures show only whole.** A picture is drawn (its file written as it
  is) only when all of it fits in its tile; one cut by the tile's edge, the
  bar or the top of what shows is left blank.
- **Keys typed while the keys load wait** in the terminal until the library,
  the default keys and your config have loaded (each part within its time).
- **Workspaces are directories.** One viewer shows one workspace; there is
  no workspace rename (`C-b w r`), reorder (`C-b w m`), or moving a pane to
  another workspace (`C-b a w`, `C-b a W`): a pane's workspace is where it
  started. The list and next/previous go by name.
- **No swap.** `C-b a s` is gone; `move PANE DIR` swaps with a neighbour.
- **No border dragging with the mouse.** A click focuses; borders move with
  `C-b r h j k l` or `border PANE DIR CELLS`.
- **A pane's kill ends the panes started inside it.** On Linux a holder
  adopts what its program leaves behind, and `kill` ends it all: a viewer run
  in a pane of a session starts its new panes from inside that pane, so
  killing that pane ends them too. Run the viewer in your own terminal, or
  start panes with `fux3 start` from outside.
- **Scrollback is a pager.** `C-b c` pages the pane's `history` in a pane of
  its own (`$PAGER`, else `less +G`), which ends itself with the pager. There
  is no copy mode and no editor history.
- **The cursor's shape** is not in the pane files: the viewer shows the
  terminal's default.
- **Pane names, not titles.** Panes are not renamed (`C-b a r` is gone); tabs
  and workspaces are.
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
| a copy mode | `history` in a pager |
| mouse reporting to programs | the viewer takes the mouse for itself |
| respawn, dump and replay, crash takeover | none |
| Windows (fux) | Linux and macOS only |
| the raw output log | none: `history` is plain text (see above) |
