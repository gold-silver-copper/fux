# fux

A terminal multiplexer that is a view over directories.

- **A pane is a directory kept by one small process** (its *holder*). The
  holder owns the program's terminal and writes what it shows into files; it
  takes typed bytes and commands from two FIFOs. There is no server and no
  socket: `cat`, `ls`, `printf`, `rm` are the clients.
- **A workspace is a folder; panes are folders in it.**
- **The viewer is one more client.** It draws a workspace's panes in tiles and
  tabs as a `layout` file says, and sends keys to the focused pane.
- **A key binding is a shell script.** A directory of them is the key table;
  a directory inside it is a menu.

fux runs on Linux and macOS with no external dependencies. Run `./sizes.sh` for the current line counts.

```sh
cargo build --release                       # fux builds entirely from this tree (three crates, no others)
export PATH="$PWD/target/release:$PATH"     # so that `fux` below is this one
D=$(fux dir); fux start build -- sh       # a pane is a directory kept by one small process
printf 'echo hi\n' > "$D/main/build/in"; cat "$D/main/build/shown/screen"     # plain tools are the clients
ls "$D" "$D/main"                           # workspaces are folders, panes are folders in them
fux view                                   # tiles, tabs, workspaces; vim, less and htop work in a pane
# a key is a script: C-b then `v` runs an executable file you can read and replace
cat ~/.config/fux/keys/C-b/v         # or under $FUX_KEYS or $XDG_CONFIG_HOME, if you set them
echo kill > "$D/main/build/ctl"             # hangs up the program; what Unix does next is what happens
```

`./example.sh` runs the first lines for real; CI runs it on both systems.
Two notes on it: the screen is written a few milliseconds after the program
prints, so a script looks again until it sees what it waits for; and the key
scripts are written on the first `fux view`, to the keys directory ("Keys are
scripts" says where that is).

## Commands

| | |
|---|---|
| `fux dir` | prints the session directory |
| `fux start [NAME] [-- PROGRAM…]` | starts a pane and prints its directory. NAME is `PANE`, `WORKSPACE/PANE`, or `WORKSPACE/` or nothing for the first free number, 1 to 9999 (no workspace named: below). No program: `$SHELL`, else `/bin/sh`. A pane whose holder is gone is started again in place; a start that fails leaves the folder, with a `log` that says why |
| `fux wait PANE-DIRECTORY` | waits until the pane's holder is gone, then prints `pid N` and `state …` as `status` and `state` have them. A holder that ended by itself wrote the last screen first (`log` says if it could not); one that was killed leaves the files as they were, `state running` too. A start in place whose holder ended before it wrote its own `status` leaves the last program's `status`, `state` and screens (its `cwd` only if the holder ended before it was about to start the program; `log` says why it ended). One whose holder ended later, before its first screen was written, has the new `status` and never the earlier program's end beside it: no `state` (`fux wait` says so and fails) or, as after a kill, `state running`. Wait with this, not by watching `state`, before you read a program's last screen |
| `fux view [WORKSPACE]` | the viewer |
| `fux layout [EDIT…]` | prints the workspace's layout as the folders make it (and writes it, if they changed it), or edits it (below) |
| `fux --help` | prints the command summary and the complete one-page guide (for people, scripts and agents): how to start a pane, type, read, wait, get a command's exit code, tell a live pane from a dead one and clean up, in plain sh, with the hazards said. It is `src/help.txt`, in the binary; its indented lines are run, in order, as one sh session by `tools/help-agents.sh` (CI, both systems; the perl section where perl is) |

Names are ASCII letters, digits, `.`, `_` and `-`, at most 64, not starting
with a dot, and not `-` alone, `layout`, `layout.new`, `colours` or `keyboard`
(a workspace and a session have files of those names).

With no workspace named, a command means the one in `FUX_WS` (a key script is
told the workspace shown there); else, typed in a pane, "here": the workspace that pane
is in (and that pane's session, unless `FUX_DIR` says otherwise); else `main`. So the
example above, typed in a pane of another workspace, starts `build` there and not in `main`.

## A pane's files

| File | What it is |
|---|---|
| `in` | FIFO. Bytes written here are typed to the program |
| `ctl` | FIFO, one command a line (a line over 1,280 bytes, counted as they were written, is dropped, with a line in `log`): `kill` (hang up the program), `resize COLS ROWS [CELL-WIDTH CELL-HEIGHT]`, `notify TEXT` (have something noticed, as a program does: `printf 'notify %s\n' 'the build is done' > ctl`) |
| `main/`, `alt/` | a folder for each of the terminal's two screens, the same five names in each: `screen`, `screen.styled`, `lines`, `links`, `pictures` (below). `main/` is there always: the main screen, the one a shell prints on and whose rows go to `history`. `alt/` is there while a program has the alternate screen (an editor, a pager), and after a pane ended under one. A name never changes which screen it means: `main/screen` is the shell's screen also while vim covers it, so what was on the terminal before an editor or a pager was opened is read without leaving it; its files are written again only when it changes (a resize wraps it anew under the program; a colour the program sets for its pane is one in `main/screen.styled` too), not when the program draws on the other |
| `shown` | a link to `main` or to `alt` (relative, so it holds in a copy of the folder): the screen that is drawn. `cat shown/screen` is what the pane shows, `readlink shown` says which screen that is. Replaced in one step, as a file is: when a program takes the alternate screen, `alt/` is filled first and `shown` turned to it after; when it leaves, `shown` is turned back first and `alt/` removed after, with all that is in it (a reader standing in `alt/` then is told: "No such file or directory") |
| `screen` (in a screen's folder) | that screen as plain text: one line per row, and nothing else |
| `screen.styled` (in a screen's folder) | the same with colours: text and SGR codes only, so `cat` shows it and nothing a program printed can reach your terminal as a control code. Where two neighbouring characters would read as one (two halves of a flag a program put side by side), U+200B stands between them. The cell a wide character left empty at a row's end, having no room there, is no text; where it has a colour it is told all the same, as U+200B and a blank at the row's end, so that the row is coloured to its last cell |
| `generation` | one number, for a reader of more than one file (below): odd while the holder is writing the pane's files, even when they agree. It grows by one each time, is replaced in one step, and goes on from where it was when a pane is started again in place. It is the last file written in each round: to follow a pane, watch it |
| `size` | `COLS ROWS` (`read cols rows < size`) |
| `cursor` | `ROW COL shown\|hidden SHAPE`: the cursor of the screen shown (a terminal has one), row and column from 0, and the shape's number as a program sets it (0 to 6); then, if the program gave the cursor a colour (OSC 12), a fifth word, `#` and six hex digits |
| `modes` | one line of words, empty when none: how input is to be sent to the program. `appcursor`, `keypad`, `paste`, `focus` (it asked to be told when its pane gains and loses the keys), `mouse9`, `mouse1000`, `mouse1002` or `mouse1003` (the mouse reports the program asked for), `mousesgr` (their newer form), `keysN` (the keyboard flags in force for it, below) |
| `state` | what became of the program: `running`, `hungup`, `exited CODE` or `killed SIGNAL` (`lost ERROR` if the holder could not wait for it) |
| `status` | `holder N`, `pid N` (the program's number) and `command …` (its first 4,096 characters): written once, when the pane starts |
| `lines` (in a screen's folder) | how that screen's rows are lines, a fact a line (`{ read -r top; read -r wraps; } < main/lines`, or `sed -n 2p main/lines`). The first line: how many lines are before the screen's first row, which is the number, from 0, of the line that row is part of. In `main/lines` they are lines of the pane's text, counted since the pane began (after `history` has become `history.1` twice, the oldest of them are in neither file). In `alt/lines` it is `0`: the alternate screen is in no text, nothing of it goes to `history`, and its lines are counted from its own first row (a number and not a sign, so that one rule reads both folders: a row's line is this number plus how many of the rows from 1 to that row do not go on). The second: the screen's rows, numbered from 0, that go on from the row above (the text wrapped into them; in `main/lines` row 0 too, from the last line of `history`), as numbers with a blank between, empty when none. Written when either line changes. A row that goes on in the next keeps the blanks at its end in `screen` and in `screen.styled`, so joining the two gives the line as it was printed (of `screen.styled`, once the U+200B and the blank of a cell a wide character left empty are taken from the row's end, as `keys/.text` does; a line feed onto a row that goes on does not end the line above, as in tmux: a shell moves down a wrapped command with one. A program that writes two lines over a wrapped one without erasing it leaves them one line): with it a script can copy a wrapped line whole |
| `marks` | where a shell that marks its prompts (OSC 133) put its marks, a line each: `LINE AT KIND`, KIND being `prompt`, `command` (where what is typed begins), `output` or `end` (with the command's exit code after it, if the shell gave one from 0 to 255). LINE is counted from the pane's first line, as the first line of `main/lines` is, a line being what was printed up to its line break however many rows it takes: the line of that number is the screen's first, a smaller number is in `history`, in `history.1` or older than both. AT is how many bytes of the line's text are before the mark, as `history` and `main/screen` (its rows joined as `main/lines` says) have that text (and the blanks `screen` cuts from a row's end, for a mark that stands past them): `cut -b` and `LC_ALL=C awk` cut there, and never inside a character. In a file made of `history.1`, `history` and the main screen, one after the other (rows that go on joined), the line LINE is line LINE + 1 + N - T, sed's count: N the line ends `history.1` and `history` have, T the first line of `main/lines` (`keys/.text` makes that file and prints 1 + N - T); below 1, it is older than the files. A mark stays on its text: when lines are scrolled, inserted, deleted or wrapped anew its numbers follow. What is erased takes its marks to where the erased part begins, in their order (so does a prompt drawn again higher up, for the marks after it): an output that was cleared away is a pair of marks at one place, and nothing. The marks are in the pane's text, so they are at the pane's root and not in a screen's folder: the alternate screen has none, and `marks` are the shell's while it is shown. Where a line that never ends was ended in `history` (below), the marks after that count from there, in the next line. The last thousand; empty when the shell marks nothing |
| `links` (in a screen's folder) | the links on that screen (a program makes one with OSC 8, as `ls --hyperlink` does), a line each: `LINE COL LEN ADDRESS`. LINE is the line's number as that folder's `lines` counts (in `main/links`, as in `marks`; in `alt/links`, from 0 at the alternate screen's first row), COL how many cells from the start of the line's first row on the screen the link begins (a whole row for each row the line has above it there), LEN how many cells it has, in reading order (a row counts as many cells as `size` says, also where a wide character that did not fit left its last cell empty, which `screen` does not show; a wide character is two cells); a link that goes over a line break is a line for each line. ADDRESS is printable ASCII without blanks, 2,048 bytes at most: anything else is no link, an address with a control byte in it among them. A cell keeps its link when it scrolls on the screen and when the text is wrapped anew, and loses it when it is erased or written over; text in `history` has none. The main screen's links stay listed in `main/links` while a program covers it. At most a thousand different addresses at a time (one more is no link, with a line in `log`), and a list of 256 KiB at most (the runs past that are left out); empty when there are none |
| `pictures` (in a screen's folder) | a line `pic-N.sixel ROW COL COLS ROWS CELL-WIDTH CELL-HEIGHT` for each picture that has a row on that screen: the file (in the pane's folder: a picture's bytes are the pane's, where it is placed is a screen's), where its first row and column are on the screen, its size in cells, and the size in pixels of the cells it was laid out in (what the program's terminal was told then: 10 by 20 until a `resize` says otherwise). COLS and ROWS are the whole picture's, also where it goes past the screen's right edge or its last row; ROW is below 0 when rows of it have scrolled off the top (`-2`: its third row is on the screen's first), and those rows are CELL-HEIGHT pixels each, whatever a reader's own cells are: the viewer cuts by that number, so one picture is cut the same in every terminal. The holder writes all seven; the viewer reads the first three and, of a picture with rows above the screen, the last, and measures the picture itself, so it draws a line of a `pictures` you wrote that has only the first three and a ROW of 0 or more. The main screen's pictures stay listed in `main/pictures` while a program covers it |
| `history` | plain text of the lines that scrolled off the top: whole lines, however the screen had wrapped them (the last may be unfinished: its rest is on the screen still). Past 4 MiB it becomes `history.1` and a new one starts, where a line ends. A line that never ends is ended when a file is past 8 MiB: the text gets a line's end there that the program did not print (in both files, with a line in `log`), what follows is the next line, and `main/lines` and `marks` count so: the files hold whole lines whatever a program prints, and a mark points at text that is there. That is looked at when lines are written, so a file gets longer than that (Bounds says by how much), and not while its last rows are those a resize sent there and may take back. When what the last line went on in is erased from the screen, the line is ended in `history` too |
| `history.styled` | the same lines in the same order with their colours: text and SGR codes only, as `screen.styled` is beside `screen` (each row a line was wrapped in starts and ends in the default style; a blank that has a colour is kept at a line's end; U+200B where two characters that were two cells would read as one, as in `screen.styled`, also where two rows are joined; no links). Without the codes, those U+200B and the blanks at line ends it is `history`: `marks` count bytes in `history`, not in this one. It becomes `history.styled.1` when `history` becomes `history.1`, at the same line, which is when either file is past 4 MiB: lines full of colour codes are put aside sooner. A colour the program set is the pane's here too: a row is told as `screen.styled` had it when it left the screen (with a background of the program's own it is as wide as the pane was), and it is a record, not told again when a colour is set or given back later; only what a resize takes back is told anew, in the colours of then. What a resize takes back from `history` is taken back from it too. What cannot be written to one of the two files is lost to that file and `log` says so: the other has those lines all the same, and what a resize sent to history just before is then not taken back from it. A file that cannot be put aside (its `.1` cannot be replaced) begins anew: its lines up to there are lost, `log` says so, and the other is put aside as always. A line without colours is its text a second time (`seq 1 2000000` writes 14 MiB to each file) |
| `pic-N.sixel` | a picture the program drew, whole, for as long as a screen places it: `cat` it in a sixel terminal |
| `title` | the window title the program set (OSC 0 or 2), one line, at most 256 characters, control characters taken out; rewritten only when it changes, and not there while the program has set none, or an empty one |
| `clipboard` | the bytes the program last put on the clipboard (OSC 52), at most 1 MiB, replaced in one step: `cat clipboard \| wl-copy`. A program that asks to read the clipboard is refused, with a line in `log` |
| `cwd` | the directory the program says it is in (OSC 7, which shells can be set to send at each prompt), one line. Not there until it says; removed when it names another machine (a shell at the far end of ssh). At most 4,096 bytes. A shell that does not send OSC 7 can write the file itself: `pwd > "$FUX_PANE/cwd"` |
| `notice` | what was last to be noticed: a number that grows with each round of writing that had one (from 1 again when the pane is started again), then the words, on one line (at most 1 KiB, control characters taken out); the number alone is the bell. From the program (BEL; OSC 9 with words, not with a number and `;` first, which is a progress bar's and its like; OSC 777; OSC 99 with plain words) or from anyone through `ctl`. The file is replaced each time: what is unread is each reader's own to remember |
| `log` | what this holder refused or lost and why (a command it does not know, a picture that is too large, history it could not write); stops at 64 KiB |

**One fact, one file.** A screen's text is text and nothing else; everything
else about a pane is a small file of its own. The files are flat, but for a
folder for each screen: the one thing a pane has two of that has several
files. A dot and a word after a name say only how a file is told or how old
it is (`.styled`, `.1`, `.new`, a picture's format), never which thing it is
of. Each file is written when it changes and
not otherwise (`clipboard`, `cwd` and `notice`: each round in which the program said one, the last it said in it), replaced in one step (written as `NAME.new`, then renamed),
so a reader sees a whole old one or a whole new one (`history` and `history.styled` are added to
in place, and a picture's file is written in place before `pictures` names it). A file that could not be written is written in the next round, which the
program's next output, a line on `ctl` or the program's end brings; a clipboard, a directory, a
notice or a picture that a failed round did not write is lost, as history is; `log`
says so for each round that failed.

**Files that agree.** One file read by itself needs nothing more: it is
whole, and of one moment. Two files read one after the other may be of two
rounds of writing: a `main/lines` one scroll behind its `main/screen`, a
`cursor` of the frame before. `generation` is for that. The holder makes it
odd before it changes anything in a round (history it adds to between two
rounds too), and even, the next number, after everything else, when the
round ended well. So read it, read your files, read it again, and keep what
you read only if it was the same even number both times:

```sh
# together DIR COMMAND…: runs COMMAND until what it read of the pane DIR is of one round (100 tries)
P=$(fux dir)/main/build     # a pane's folder: here that of the example at the top
together() { d=$1; shift; tries=0; until g=$(cat "$d/generation") && [ $((g % 2)) -eq 0 ] && "$@" && [ "$g" = "$(cat "$d/generation")" ]; do tries=$((tries+1)); [ $tries -lt 100 ] || return 1; sleep 0.05; done; }
first() { top=$(sed -n 1p "$P/main/lines") && row=$(sed -n 1p "$P/main/screen"); }
together "$P" first && echo "line $top of the pane's text is: $row"
```

Resolve `shown` inside the read (`cat "$P/shown/screen"`), not before it, and
have the path name the same pane throughout (`together` keeps what it knows
in `d`, `g` and `tries`, which its command must leave alone, as it must not
call `together` itself; and it runs the command anew each time: a command
that writes a file writes it anew). The number speaks for what the holder alone writes: not for `cwd`,
which is the program's to write too and goes when a start in place begins,
nor for `log`, the FIFOs or a script's own files. What is lost for good
does not keep it odd: history a file could not take is a line in `log`, and
the round ends. A round that cannot begin (the odd number cannot be written:
`log` says why) changes no file, and the number stays the even one it was;
history waits meanwhile, up to the 4 MiB of "Bounds". After a round that
failed once it had begun the number stays odd until a later round puts it
right, and after a holder was killed in the middle of one it stays odd for
good: so a reader gives up after some tries and takes
the files as they are, as `keys/.text` does. A folder copied while its pane
was busy has no such promise (copy it under the same check). The viewer
reads a pane when its `generation` is another file than at the last look,
keeps what it read only if it is still that file afterwards, and does not
read while the number is odd and the holder lives; so a folder you made by
hand is drawn at once, and drawn anew when you write a `generation` into it.

**Bounds.** Each round replaces `generation` twice (as it begins and as it ends) and, between the two, the files that changed: the shown screen's `screen` and `screen.styled` as a rule, `lines` when its numbers did (as when a line scrolls off), and, when lines wait for it, an append to `history`.
While a program keeps writing (never a millisecond without output) that is at most
40 rounds a second: a flood (`yes`) costs 80 replacements of `generation` and 80 of screens a second. What a program
wrote before it wrote nothing for a millisecond is shown at once, though never within 8 ms of
the last round: a program that writes a little every few milliseconds costs at most 125 rounds
a second. Nothing is synced to disk. The holder reads at most
512 KiB of output between two rounds (while a program is in the middle of drawing, below, in
any 25 ms), and past that the program waits, as on any slow terminal. The holder keeps at most 64 KiB of typed bytes for a program that is not
reading; past that it stops reading `in`, and once the FIFO's own buffer
(the kernel's) is full, writers to `in` wait. A screen is at most 500 by 500
cells. A program that says it is in the middle of drawing (mode 2026) is not
written out until it says it is done, or what is not yet written (output, or a line on `ctl`) has waited half a second (history may be added to meanwhile): so one that draws frame after frame without a pause is shown about twice a second, in the middle of a frame if that is where the half second ends. A screen keeps at most 8 pictures of at most 4 MiB of data and 500 cells each way; a ninth drops the
oldest, with a line in `log`. At most 4 MiB of history and one row wait to be written,
counted on the longer of `history` and `history.styled`: rows past that (one
write of the program or one resize that makes more, with what still waits from before; rows a resize takes back and gives again are counted once) are not kept, in either
file, with a line in `log`, and the line they were part of ends there in
both. The holder writes what waits once it is past 512 KiB, so a history file
is 4 MiB and one such write long at most: about 4.5 MiB as a rule, and past 12 MiB in the worst cases (two writes of 4 MiB landing after the last one that fitted, or a line that never ends), and the screen's text more
while a resize may take its rows back. When the program ends, at most 4 MiB of what
it wrote last is still read; a `resize` first draws, at the old size, up to
1 MiB of what the program had already written.

**How fast.** `tools/perf.py` measures fux beside tmux on the same steps, each
in one 200x60 pane seen through its viewer in a pty the script owns: a flood,
a key until its echo, the CPU at rest, a program that repaints every cell,
and a key while the viewer's output is read slowly. It needs python3, takes
about a minute and a half on two cores, and leaves nothing behind:

    cargo build --release && tools/perf.py target/release/fux

One run of 2026-11-20, at 5a5cdbea8, on the machine this was written on (Linux, arm64,
two cores of twelve, tmux 3.8): the median of each (least to most), before the changes it was written
for (at 21cf936b4) and after them. fux has changed since: the script gives today's.

| | fux before | fux after | tmux |
|---|---|---|---|
| `seq 1 2000000`, until its end is on the screen | 4.37 s (4.37 to 4.41) | 1.44 s (1.43 to 1.44) | 0.99 s (0.99 to 1.00) |
| A key until its echo, typed slowly | 9.5 ms (5.6 to 13.2) | 4.1 ms (1.8 to 5.4) | 0.13 ms (0.03 to 0.32) |
| A key until its echo, typed fast (10 ms apart) | 20.1 ms (6.3 to 33.5) | 5.2 ms (2.1 to 6.5) | 0.11 ms (0.02 to 0.25) |
| At rest: the viewer (tmux: the client) | 3.2% of a core (2.6 to 4.3) | 0.12% (0.12 to 0.13) | 0% |
| At rest: the holder (tmux: the server) | 0.001% of a core | 0.001% | 0% (0 to 0.04) |
| All 200x60 cells repainted: frames the program wrote | 704 a second (702 to 707) | 702 a second (698 to 703) | 948 a second (947 to 962) |
| Meanwhile: the viewer (tmux: the client) | 27% of a core (25 to 29) | 13% (13 to 13) | 0% |
| Meanwhile: the holder (tmux: the server) | 55% of a core (52 to 57) | 54% (54 to 55) | 98% |
| Read at 100 KiB a second: a key until the program reads it | 52 ms (0.2 to 264) | 0.17 ms (0.05 to 3.4) | 0.11 ms (0.07 to 1.8) |

(The middle and right columns are that run; the left column is the same steps on the
binary built at 21cf936b4. CPU is the time the scheduler says all of a process's threads
ran, from `/proc/PID/task/*/schedstat` (the script takes `ps`'s where there is none); the
left column's is that of each process's first thread, which in that binary did all
the work. tmux's server writes to the terminal itself, so its client does nothing; and
fux's holder reads only so much between two screens (Bounds), which is what bounds
the frames a program can write. From run to run the key rows move by a millisecond and the flood by a few
hundredths of a second.)

The program runs with `TERM=xterm-256color`, `TERM_PROGRAM=fux`,
`TERM_PROGRAM_VERSION`, `COLORTERM=truecolor` and `FUX_PANE` (its pane's directory, whole path), in
the directory `fux start` was run in, with the rest of that environment and
the signals 1 to 31 at their defaults. Four names are not passed on, those a key script is
told of its viewer (`FUX_WS`, `FUX_VIEW`, `FUX_ROW` and `FUX_COL`: below);
and neither `COLUMNS` nor `LINES`, which the holder reads for itself: the
pane's first size is 80x24, or the `COLUMNS` and the `LINES` the starter set (each 1 to
500; one that is set and is not such a number fails the start, which says so).

What is this terminal, and what can it do? Programs ask, and fux answers as
itself and for itself, with nobody watching:

| Asked | Answered |
|---|---|
| `CSI > q` (XTVERSION) | `fux VERSION` |
| `CSI c`, `CSI > c` (device attributes) | `CSI ? 62;4;22 c` (a VT220 with sixel and colour) and `CSI > 70;0;0 c`: a kind of terminal no other gives (xterm is 41, tmux 84, Ghostty 1 with version 10), so nothing a program knows of another terminal's versions is turned on |
| `DCS $ q … ST` (DECRQSS) | what the emulator keeps: `m` (the colours and looks in force, in the form with colons), `r` (the scroll region), `SP q` (the cursor's shape); "not valid" for anything else |
| `DCS + q NAMES ST` (XTGETTCAP) | each name by itself, from the table below; "not valid" for every other name, so a name fux does not know never hides one it does (answers past the 16 KiB of them the emulator lets wait at once are dropped, with a line in `log`; what waits for the program, typed bytes and answers together, is 64 KiB at most) |

| Name | Value |
|---|---|
| `TN` | `xterm-256color` |
| `name` | `xterm-256color` |
| `Co` | `256` |
| `colors` | `256` |
| `RGB` | `8/8/8` |
| `Tc` | yes |
| `setrgbf` | `\E[38;2;%p1%d;%p2%d;%p3%dm` |
| `setrgbb` | `\E[48;2;%p1%d;%p2%d;%p3%dm` |
| `Smulx` | `\E[4:%p1%dm` |
| `Setulc` | `\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm` |
| `Ss` | `\E[%p1%d q` |
| `Se` | `\E[0 q` |

That is all fux answers by name: true colour, the kinds and the colour of an underline, the
cursor's shape (`Se` gives it back to your terminal's own). Setting the
clipboard works (OSC 52 with `c`) and is not in the table: the capability's
name, `Ms`, promises a choice of clipboard and a way to read it, and fux has
neither. All it does is under "Whose rules it
follows". What it leaves out (left and right margins among them: `CSI ? 69 $ p` is
answered "not known"; of the modes that are not private, `CSI Ps $ p`, insert mode is told as it is and every other as not known, the line feed mode too, which xterm tells) a program that asks first does not turn on. A pane's program also has
`COLORTERM=truecolor` in its environment, which is what many read instead of
asking. `TERM` stays `xterm-256color`: fux has no terminal description of
its own to install, and claims no other terminal's. Three things follow.
The viewer passes 24-bit colours on as they are, so on a terminal that has
none of them (few are left) run the program as `env COLORTERM= PROGRAM` (the holder sets it: before `fux start` is too early). A
program that goes by `TERM` alone may use what an xterm has and fux has not:
a nested tmux needs
`set -as terminal-features 'xterm-256color:usstyle'` to pass curly
underlines on. And an answer nobody reads is typed text, as on any terminal:
a program that asks and ends at once leaves it on its last screen (where the terminal echoes: after `stty -echo` nothing shows it), and one
that asks from a shell's prompt finds it on the command line.

What colours has the terminal? Programs ask (vim, Neovim and others, to
choose a light or a dark theme), and a pane has no terminal of its own to
know. The answer is one file, `colours` in the session directory
(`$(fux dir)/colours`), plain lines that you may write yourself:

```
foreground #d0d0d0
background #1c1c1c
1 #cc0000
```

(`foreground`, `background`, `cursor`, or a palette number from 0 to 255, then `#` and
six hex digits; of two lines for one colour the last counts, also when its value
is none, which is no answer; any other line counts for nothing, and 16 KiB of the file are read). The
viewer asks its terminal once when it starts and, when the terminal has
answered (or ten seconds have gone by, on a slow line), writes the file with what it was told, in place of the one that was
there, yours too; a terminal that tells of no colour leaves the file as it
is. Two viewers on terminals of different colours do not agree: the one that
wrote last is believed. A pane's holder answers a program's question (OSC
10, 11, 12, and 4 with `?`) from that file, with or without a viewer, and says
nothing about a colour the file has no line for: a program is never told a
colour that someone made up.

A colour a program sets is its pane's, and no other's. It sets one with OSC
4 (a palette number, any number of pairs in one string), 10 (the
foreground), 11 (the background) or 12 (the cursor's) and a value: `#` and
3, 6, 9 or 12 hex digits, or `rgb:R/G/B` with one to four hex digits each.
Each part is kept as eight bits (the first eight of `#` digits; `rgb:f/f/f` is white). What sets nothing is said in the pane's
`log`: a value that is none of these (a name such as `red`, `rgbi:` and the
other forms X has, no value at all) and, in OSC 4, a number that is not 0
to 255. A question about such a number is passed over without a word, as is
a number that has no value after it; and one string may ask more than is
answered (the answers past those that may wait are dropped, as above): what
it sets is set all the same. From then on:

- the program is told what it set when it asks, the file or no file;
- the pane is shown in it. `screen.styled` (and so the viewer, and the last
  screen a program leaves when it ends) gives a cell that has that palette number,
  or the default foreground or background, the colour that was set, as
  direct colour (`38;2;R;G;B`, `48;2;R;G;B`): text that was on the screen
  before as well, since the emulator keeps a cell as the program wrote it
  (and tells DECRQSS so). With a background of its own every cell of the
  pane that has the default one has it, the blank ones too: each row of `screen.styled` is then as
  long as the pane is wide (the cell a wide character left empty at a
  row's end among them), and the tile is that colour to its edges. The
  plain `screen` and `history` have no colours and do not change;
- the cursor's colour is the fifth word of the pane's `cursor` file, and the
  viewer gives its terminal's cursor that colour (OSC 12) while the pane has
  the keys and its program lives. When another pane has them, a menu is open
  or the viewer leaves, the terminal's cursor has its own colour again (OSC
  112), which is the one the terminal is set up with: a colour another
  program gave the cursor before the viewer started is not put back, as its
  shape is not;
- the program gives a colour back with OSC 104 (the whole palette, or the
  numbers it names), 110, 111, 112, or a full reset (`ESC c`): then a program that asks is told
  what the file says again, and the screen is drawn in your terminal's own colour (the two differ only when the file says another colour than your terminal has).

The pane has one set of colours for its main and its alternate screen: the
256 palette numbers and these three, no more. The viewer never sets a colour of your terminal other than the
cursor's: a theme a program chose in one pane does not repaint the pane
beside it, and no file is needed to learn a pane's colours, as its cells
carry them.

How are keys told? A terminal tells Tab and Ctrl-I, Enter and Shift-Enter,
Escape and the start of an arrow key with the same bytes, unless the program
asks for the kitty keyboard protocol. A program in a pane may ask: what it
asks for is kept by the pane's holder as the protocol says (set, set aside,
taken back and asked about, apart for the main and the alternate screen, 16
deep), answered with nobody watching, and shown as `keys…` in `modes`. All
five of the protocol's flags can be in force: 1, escape codes told apart; 2,
a key's press, repeat and release; 4, alternate keys; 8, every key as an
escape code; 16, the text a key gives. `keysN` is their sum.

The keys come from your terminal, so no more is put in force than it takes.
That is in a second file of the session, `keyboard`: one number, the flags
the terminal takes. The viewer asks its terminal when it starts and writes
it (`0` if the terminal does not know the protocol; the viewer that wrote
last is believed); `echo 5 > "$(fux dir)/keyboard"` writes it by hand. What a
program sets is in force as far as the file says then, and that stays true
for it: with no file, or `0`, it is told that no flag is in force and gets
keys the old way, also after a viewer on a better terminal has come (start
the program again to have them). While a pane has the keys, the viewer asks
its terminal for that pane's flags, and takes them back when the focus moves
or the viewer leaves. The keys go to the program as the terminal sends them. (What
a program was told stays what `modes` says, also when a viewer on a lesser
terminal comes later: that terminal sends what it can.)

With flag 2 the terminal tells of keys held and let go, and the viewer sees
those too. A key belongs, until it is let go, to whoever got its press. A key
the viewer took for itself (the prefix, a key of a menu) is its own to the
end: its repeats and its release reach no program. (Held in a menu that
stays open, as the resize menu does, a key that did something there does it
again, while that menu is the one open: in no other.) A key that went to a
pane is that pane's: its repeats and its release go there too, also while a
menu is open, unless the pane's program has ended, been started anew or
stopped asking meanwhile (the pane's `status` and `modes` are read then).
When the keys leave a pane with a key of its still down (another pane or
workspace gets them, or the viewer leaves), the viewer tells that pane
itself that the key is let go, since the terminal, asked for the next pane's
flags, may never tell: a press is followed by its release. What the terminal
still tells of that key in the protocol's form reaches no pane; if the next
pane asked for no flags, the terminal sends a held letter as plain letters,
and those are typed there as they would be without fux. Only a press makes a
key someone's: a repeat or a release whose press nobody got goes nowhere.
While a menu is open, Shift, Ctrl and their like by themselves, and the
locks, do nothing: they neither choose nor close it. (The viewer knows of
the last 64 keys that are down.)

The viewer reads both forms and gives a key one name in both, so `C-b` and
the keys in the keys directory are found whatever the focused program asked
for, and a key that runs a script or opens a menu is not also sent to the
program. Held keys are named in the order `C-`, `M-`, `S-` (`C-M-x`); a sign
that needs Shift is named by the sign (`M-#`) when the terminal says which
sign it is, which it does under flag 4 and not under flag 1 alone (there the
key is `M-S-3`); the keypad's keys are named as the keys they double (in the old form, those with codes of their own have no name and go to the program). Two
keys that the old form cannot tell apart have two names in the new one:
Ctrl-I is `C-i` and no longer `tab`, Ctrl-Backspace is `C-backspace` and no
longer `C-h`. Who has the keys is read from `layout` when keys come, so a
key typed right after `fux layout focus …` is the new pane's; until the
viewer has drawn once more, it may reach that pane in the form the last one
asked for.

Is a pane alive? Its holder keeps a lock on the directory for as long as it
lives: `flock -n "$D/main/build" true` fails while it does (Linux), and
`fux wait` returns when it is gone.

## The viewer

`fux view [WORKSPACE]` shows every folder in the workspace whose name is a
pane's name (above) as a pane. The
`layout` file in the workspace says where; a person can read and edit it, and
viewers of the same workspace share it, focus included:

```
* row(sh col(logs top):2) focus=logs
- vim focus=vim
```

One tab a line. `row(…)` puts its parts side by side, `col(…)` stacks them,
`:N` is a part's share (1 to 10000; rows and columns nest at most 32 deep), `*` marks the tab shown and `-` the tab shown before it, `focus=` is the pane that
gets the keys. When the tab shown goes, the `-` tab is shown, as `cd -` would,
or with none the tab now in its place (a tab goes when its last pane's folder is removed,
not when a program merely ends); an edit that ends on another tab moves the mark. It
is in the file, so two viewers agree. A folder the file does not place gets a
tab of its own; a name with no folder is dropped. A part there is no room
for (each needs a cell, and one for the line before the next) is hidden. The focused pane gets a cell even
then. A workspace shows at most 1,000 panes. `fux layout EDIT` makes
one edit:

`split row|col PANE [beside ANCHOR]` (put PANE beside or below ANCHOR, defaulting to the focused pane) ·
`pane PANE` (give it the keys, and show its tab) · `focus left|right|up|down|next|prev` (a side is a side in the terminal whose size the edit is told in `COLUMNS` and `LINES`, as a key's script is; told none, in a very large one) · `tab next|prev|-` (`-`: the tab shown before) · `break`
(the focused pane to its own tab) · `swap next|prev` · `size row|col +N|-N` ·
`rotate` · `arrange row|col|grid|main`

(`size` changes a share, counted in hundredths of its row or column, not a number of cells: the cells come from rounding the shares along the row. Where
one part has nearly all of it, or a share is rounded to a whole hundredth, a step can leave the line where it is or move it a cell or a few the other way
(`row(a:6 b:113)` in 120 columns, `-5`: a has 7 where it had 6). Step again, or write the shares in the file.)

Your terminal may not count a character's columns as fux does (it may draw
three people where fux joined a family). The viewer keeps that inside the
cell: after anything but plain ASCII it places the next cell by row and
column, it sends the cells such a character may have been drawn over again,
and where one could reach past its tile's edge it draws a `?` in each of its
columns instead (the pane's `screen` keeps the character): a family with fewer
than six columns left in its tile is `??`, since a terminal that does not join it
draws three people there. The viewer asks your terminal once whether it joins
characters as fux does (mode 2027), turns that on if it is off, and draws no
`??` if the terminal says yes. A terminal set to
draw "ambiguous" characters two columns wide is not catered for: the lines
between tiles are such characters.

A program that measures text otherwise than its terminal draws it wrong in
any terminal: bash counts each part of a joined character, so its cursor
is off after an emoji with a skin tone here as in tmux, and stepping into a
Devanagari conjunct, which is one character here, redraws it oddly.

The viewer looks at the `notice` of every pane of the workspace it shows,
four times a second. A new one is passed on to your terminal once (the bell
as a bell; the pane's name and the first 200 characters of the words as OSC 9), said in the
corner, and its tab has a `!` in the bar (which is there while a prefix is
held: `C-b`) until its pane has had the keys. Of two of one pane between two
looks the last is passed on; of two panes', both, the last in the corner. Another workspace's notices are not looked at. What is there at the first look at a workspace only marks its tab. A program
that asked to be told about focus (mode 1004) is told when its pane gains
and loses the keys, and when your terminal's window does.

The viewer asks a pane to take the size of its tile (through `ctl`), once each time the tile, the terminal's cell size or the pane's program changes (a request its `ctl` had no room for, 64 KiB unread there with its holder stopped, is not made again until one of them changes). Two
viewers with different sizes do not fight: the last to ask wins, and the
other shows what fits.

A resize wraps the main screen's text anew: a line that was wrapped is one
line again at any width, and no character is cut in two (on a screen one
column wide, a wide character becomes a blank). Rows that no longer fit go
to `history` from the top, and come back from it (the last 100,000 cells of them) at the next resize that has
room for them, if nothing has scrolled off and the screen was not cleared in between: a tile made narrow
for a moment is as it was when it is wide again, and `history` is then that
much shorter at its end. What scrolled off stays in `history`: a screen made
taller later gets blank rows at its bottom. The cursor stays on its
character while that character's row is on the screen; if the text below it
needs the room, that row goes too and the cursor is kept at the top. The
alternate screen (vim, less) is cut or padded, never wrapped, and nothing of
it goes to history: its program draws it again. A size that is refused
changes nothing and leaves a line in `log`.

### Keys are scripts

The keys directory is `$FUX_KEYS`, else `$XDG_CONFIG_HOME/fux/keys`, else
`~/.config/fux/keys`; the viewer
writes the default scripts there if it does not exist (and only then: after
an upgrade, remove the folder, or copy `keys/` from the source over it, to
get the new defaults; a copy over it leaves the keys this version no longer has). A key that has a file
of its name runs it (a file that cannot be run says so in the corner). A key that has a directory opens it as a
menu (each script's second line, `# what it does`, is its entry) and the next
key is looked up there. Any other key, with no menu open, goes to the program. So the prefix is
only a directory: `keys/C-b/v` is "C-b, then v", and `keys/M-left` would be
Alt-Left with no prefix.

Key names: a character is itself (`v`, `%`, `é`); a capital is `S-` and its small letter, in any alphabet (`S-h` is H, `S-é` is É: file
names differ by more than case, for macOS; for a letter outside ASCII, a terminal that also tells the key's letter on a US layout names it by that one); `C-b`, `M-x`, `enter`, `tab`,
`esc`, `space`, `slash`, `dot`, `backspace`, `up` `down` `left` `right`,
`home` `end` `pgup` `pgdn` `insert` `delete`, `f1`…`f12`, with `C-`, `M-`,
`S-` before them when held (`C-left`, `S-f5`).

A script gets `FUX_DIR` (the session), `FUX_WS` (the workspace shown),
`FUX_PANE` (the focused pane's directory, empty with no pane), `FUX_KEYS`, `FUX_VIEW`,
`COLUMNS` and `LINES` (the view's size, which a pane alone in a tab has; `COLUMNS` is not set for a terminal that says it has no columns, nor `LINES` for one with no rows: `fux start` takes 80 or 24 for the one missing),
`FUX_ROW` and `FUX_COL` (the size, as `COLSxROWS`, of the tile a split of the
focused pane would make: `FUX_ROW` side by side, as `C-b v` splits, `FUX_COL`
one above the other, as `C-b s` does; the split keys
start their pane at that size, so its program is not drawn twice), and this
`fux` first in `PATH`. Keys typed after it wait until it ends (two seconds
at most), so `C-b t n` and then text types into the new shell; one that
takes longer runs on while keys are taken again (the defaults put such work in the background).
Keys that waited (for a script, or for a workspace's first frame) are read as one run of bytes when they are taken: Escape and then a letter typed meanwhile are Alt with that letter, however long lay between them (in the old form of keys; in the kitty form Escape is a key of its own).
What it prints goes to the viewer, a line at a time: `quit` leaves the
viewer, `workspace NAME` shows another workspace (a name with no folder is shown too, and says that it has none), and anything else (an error
from `sh`, say) is shown in the bottom right corner until the next key (its first 200 characters; a line over 4096 bytes is dropped whole, and the corner says so). A key with no file, pressed in an
open menu, closes the menu and goes nowhere (in a menu that stays open, as resize and move do, the prefix opens its own menu again: `C-b r l l C-b v` resizes and then splits). Writing the same lines to the FIFO
`$FUX_VIEW` does the same from anywhere.

The defaults are fux 1's keys, all under `keys/C-b/`: `v` split side by side,
`s` split stacked (both, and a new tab, start where the focused pane's program says it is, its `cwd`, and else in the directory the viewer was started in),
`x` then `y` close the pane (remove its directory), `h j k l` focus,
`o` next pane, `n` `b` next and previous tab, `d` detach, `c` the pane's text
in the editor, `M-c` (Alt-c) the same text in colour in `less`, `C` (Shift-c) the last command's output in the editor (from
the last `output` mark to the `end` after it, or to the text's end, so without the prompt that
follows an output with no line break; it says so if the command printed
nothing or its output was cleared away; of a long output that Ctrl-L or `clear` cut in two,
the part that had scrolled into history is all there is, and it opens that) and `p` the editor at the last marked prompt but one, or at the only one
(both for a shell that marks its prompts, and both also while the pane shows a
program's own screen, as `c`, `M-c` and the search are: the text is the main screen's and its history, whichever screen is drawn. For bash from 4.4 on, `PS1='\[\e]133;A\a\]'$PS1'\[\e]133;B\a\]'; PS0='\e]133;C\a';
PROMPT_COMMAND='printf "\e]133;D;%s\a" "$?"'`, the `printf` put first in a
`PROMPT_COMMAND` you have already), `C-b` send C-b itself; `t` then `n` a new tab, `w`
workspaces (`n` new, `g` choose a workspace, `h` `l`), `r` resize (`h j k l`, repeating), `m` move
the pane (`h` earlier, `l` later, repeating), `a` pane actions (`n` to a new tab, `k` hang
up the pane's program), `g` arrange (`r c g m`, `t` turn), `f` find
(`p` a pane, `t` a tab, `s` search every pane: a list in a pane of its own, chosen
with fzf, sk or fzy if one is installed and by number or name if none is (a number is a line of the list first, and a name only if the list has no such line)). Whether a pane
closes when its shell exits is the script's choice too: the defaults start
not your shell but `keys/.shell`, a few lines that run it and then remove
the pane's own folder, however the shell ended. Only when `.shell` itself
is gone first (killed, or hung up) is nobody left to remove the folder, and it stays until the
close key or `rm -r` takes it: saying `killed` after `kill` in `ctl` or a
`.shell` killed outright, and `lost` when the holder was killed. The `a k`
key hangs up the pane's program (`kill` on its `ctl`, as closing its terminal
would); a signal of your choice is one line (`kill -TERM` and the `pid` of `status`). The close key moves the folder out of the
workspace (the tile goes at once), removes its `in`, and removes the folder
when `fux wait` says the holder has left (a holder looks for its `in` every two seconds).

In a menu's directory, a file `.about` holds the words shown for it in the
menu above, and a file `.repeat` keeps the menu open after each key (until a
key it has not): that is all "resize mode" is. A directory with one script,
like `x/y`, is a question.

A script's third line may be `# group: NAME` (and so may the second line of
an `.about`): the menu lists keys under their group's name, groups in
alphabetical order, keys with no group first (the menu reads a script's
first kilobyte for its caption and group). A menu taller than the
terminal goes on in more columns; what has no room is counted in its last line (a `▼` alone where the count itself has none).

There is no copy mode: `C-b c` puts the pane's older history, its history and
its screen in one private file and opens it in `$EDITOR` (which may hold
arguments, split at spaces), else `$PAGER`, else `less`, in a new tab. `vi`, `vim`, `nvim`,
`nano`, `emacs`, `less`, `hx` and `helix` are opened at the file's end (and by the prompt
keys at their line); any other editor
gets the file alone, as not every editor reads `+LINE`. An editor behind a
launcher of another name is one of those: say how yours is run in
`FUX_EDIT` (it comes before `$EDITOR`), a line of sh that is run with the file as `$1` and the line as `$2`, set where
the viewer is started (a viewer started in a pane has it too; `export FUX_EDIT='hx "$1:$2"'`, or your launcher in
`hx`'s place; `keys/.edit` reads it and is yours to change). The file is in the editor pane's own folder and is removed
when the editor ends, or with the pane if that is closed first. Its tab goes too (the
pane removes its own folder, as a shell pane does), and the
tab shown before it is shown again (the one you came from, unless you looked
at another meanwhile); an editor that failed first waits for Enter, so that
what it said can be read.

`C-b M-c` (Alt-c) is the same with its colours: the text of the styled files,
in `less -R` at its end, whatever `$EDITOR` is (an editor shows the codes, not
the colours). Its tab and its file go when you leave less (`q`), as an editor's
do, and a text shorter than the screen stays open whatever `$LESS` says (`-+F`).

There is no zoom either: a pane shown alone is a tab with one pane. If you
want the key, these two scripts are it (`keys/C-b/z` and `keys/C-b/S-z`).
The first takes the focused pane to a tab of its own and keeps its old tab's
line in the pane's folder; the second writes that line back. (The
second script edits the file by hand, which `fux layout` has no edit for, and
so takes the same lock itself: sh has no way to, and it uses perl for that
one line. Without perl, leave the line out: the edit is then a hand edit ("Don't" says what that risks).)

```sh
#!/bin/sh
# show this pane alone, in a tab of its own
# The tab's line of the layout is kept in the pane's folder; `C-b Z` puts it back.
[ -n "$FUX_PANE" ] || exit 0
[ -f "$FUX_PANE/alone" ] && { echo "already alone: C-b Z puts it back"; exit; }
fux layout | grep '^\*' > "$FUX_PANE/alone.new" || exit
fux layout break
# A pane that was alone already has not moved: there is nothing to put back.
if fux layout | grep '^\*' | cmp -s - "$FUX_PANE/alone.new"
then rm -f "$FUX_PANE/alone.new"
else mv "$FUX_PANE/alone.new" "$FUX_PANE/alone"
fi
```

```sh
#!/bin/sh
# put this pane back where `C-b z` took it from
# The layout is text: the kept line replaces the line of the tab the pane left,
# and the pane's own line goes.
[ -f "$FUX_PANE/alone" ] || { echo "this pane was not taken out with C-b z"; exit; }
ws=$FUX_DIR/$FUX_WS me=${FUX_PANE##*/}
# Under the lock `fux layout` takes (flock on the workspace's folder), by way of perl (optional: see above).
[ -n "$LOCKED" ] || LOCKED=1 exec perl -e 'use Fcntl ":flock"; open(D, "<", shift) && flock(D, LOCK_EX) or die; exit(system({ $ARGV[0] } @ARGV) >> 8)' "$ws" "$0"
awk -v me="$me" '
  function names(line, into,    n, i, w) {
    sub(/^[*-] /, "", line); gsub(/row\(|col\(|focus=[^ ]*|[()]|:[0-9]+/, " ", line)
    n = split(line, w, " "); for (i = 1; i <= n; i++) into[w[i]] = 1
  }
  NR == FNR { kept = $0; names($0, near); delete near[me]; next }
  { split("", here); names($0, here) }
  (me in here) { own = $0; next }
  !done { for (p in here) if (p in near) { print kept; done = 1; next } }
  { print }
  END { if (!done) print own }
' "$FUX_PANE/alone" "$ws/layout" > "$ws/.layout.$$" && mv "$ws/.layout.$$" "$ws/layout" && rm -f "$FUX_PANE/alone"
```

Put back: the tab, the pane's place in it, every share, the focus. Not put
back: a pane closed meanwhile (dropped), a pane opened meanwhile (it keeps
its own tab), and a share that another viewer or script changed in the old
tab while the pane was out, which the kept line overwrites. If the pane's
old neighbours are in different tabs by then, the kept line replaces the
first tab that holds one of them; with none left, the pane keeps its tab. Closing the pane
forgets the line.

**The look** is fux 1's. A pane has every cell but the one-cell line between it
and its neighbour (`│`, `─`, a tee or a cross where lines meet); the lines
touching the focused pane are green. There are no titles and no bar: while a
prefix is in use, the last row shows the workspace, its tabs and the keys
typed so far (or a message), and the corner above it the keys that can follow. A message
(a script's words, what is wrong with `layout`) sits in the bottom right
corner until the next key. A pane whose program ended, or whose holder is
gone, says so in the corner of its own last row (`exited 0`; `lost` where `state` still says `running`).

**Titles and the clipboard.** The viewer gives your terminal the focused
pane's title (and its own back when no pane has one, and when it leaves);
no row is taken for it. What the focused pane's program copies (OSC 52)
is passed to your terminal once, when its `clipboard` file changes: looking at a pane, or
coming back to it, never sends an old clipboard again. A copy the viewer could not read when it was written (a `clipboard` it may not open) is sent when it can be read and the pane's tile is next drawn: a `chmod` alone sends nothing, unless a menu is open, which draws.

**The mouse** passes through. When the focused pane's program asks for mouse
reports, the viewer asks your terminal for the same in the SGR form (and for releases too when the program asked for presses alone, mode 9, which is told of presses only) and forwards each one over
that pane's tile to it, in the form it asked for, counted from the tile's corner; a drag that leaves the tile is
reported at its edge until a button is let go (with two held, the first to be let go ends it: the rest of the drag outside the tile is not told). A click on another tile
gives that tile the keys and is not passed on. The viewer uses the mouse for nothing else, and when the
focused program does not ask, your terminal's own selection works as ever.

Pictures: when the terminal says it draws sixel, a pane's picture is drawn in
its tile. The viewer measures every picture itself, in pixels: the picture's
own size (the larger of what it says and what it paints) against the pixels
its tile has for it, counted in your terminal's cells (the cells a pane
counted it in say where it starts and, of one with rows above the pane's top, how many of its pixels those rows are). One whose pixels are square and
all in the tile is passed on as the program wrote it. Of any other (wider or
taller than the tile, with rows scrolled off the pane's top, or asking in its raster attributes for
pixels that are not square) the part inside the tile is drawn, at its place
and never over the line between tiles or a neighbour: the viewer reads the
picture (`fux-vt/src/sixel.rs`: raster attributes, colours in RGB and HLS,
repeats), takes the pixels the tile has for it and writes those as a sixel
picture of its own. The file stays whole, so the picture is whole again in a
wider tile; a file written anew under its name is drawn anew. What is drawn
of a picture is made once and kept for as long as it is drawn. A part has
square pixels, and a pixel the program never painted stays unpainted in it
(the part says that such pixels keep what is under them, whatever the
picture said: which colour a terminal fills them with is its own).
No picture is drawn on your terminal's last row: a terminal
may put the cursor under a picture and scroll everything to do so. While a
menu is open, a picture it covers a cell of is not drawn. A cell counts as
100 pixels wide and high at most, in a pane and in the viewer alike.

A picture is cut within bounds, and the bounds are on the part in the tile:
what a picture paints outside it is read (the 4 MiB a picture's file may
have, a step for each byte) and counts for nothing else. The part is at most
8,388,608 pixels (a screen of 3840 by 2160 has fewer); what is painted in it
may name colour registers 0 to 255 and 256 different colours, and may paint
each of its pixels 16 times over on average; 4 MiB of sixel are written for
it. All the pictures on the terminal are 16 MiB together at most (counted as the panes are drawn, under a menu too; one left out for this stays out when others go, until its file or its tile changes or its tab is shown anew). A picture
past one of these is not drawn (no stand-in is), and the viewer's corner
says which, until the next key: once for each part, and every one of a
frame. So is every picture in a terminal that does not say how many pixels a
cell has: nothing then tells whether one stays in its tile. One that was
refused is looked at again when its file or its tile changes, or it was not shown for a frame.
Another protocol later is another file extension beside `.sixel` and a few
lines in the viewer.

## Search every pane, and after a reboot

Both are scripts over the files, and yours to change.

**`C-b f s` searches every pane of the session.** It asks for words in a pane
of its own, lists every line that has them as they are (no pattern; each pane's `history.1`,
`history` and main screen, wrapped lines whole, ASCII letters of either case; ended
panes too; a `history` put aside in the instant it is read can be missed: search again), and opens the pane's text, made anew, at the number of the line you choose, in the editor.
`keys/.search` does it: `.text` for a pane's text, `grep`, `.pick` for the
choice, `.edit` to open.

**`sh ~/.config/fux/keys/.restore` (in your keys directory) starts every pane with no holder again**, under its name,
with the words of its command line (`status`) and in the directory the
program last said it was in (`cwd`: a shell that announces it; else, and after a start in place that failed since and took `cwd` away, your home
directory). What the pane had printed is kept beside it in `before` (`history.1`, `history` and `main/screen`, one after another), since a
start in place begins a pane's files anew, also one whose holder then ends before its first screen. A pane whose text cannot be copied (a file of it that cannot be read: `cat` says which) is not started again. Run from a login with no `FUX_KEYS`, it tells the panes the folder it is in itself. A pane that is alive is left
alone, and the layouts are files, so tiles and tabs are as they were. It
starts programs, it does not bring them back: a shell starts fresh in its
directory, an editor without its open files.
It is honest, not exact. `status` has the command as one line, its arguments
joined by blanks, and the script starts the words between the blanks as they
are, with nothing in them expanded and no shell reading them. An argument
that had a blank in it cannot be told from two, an empty one is not there,
and a quote is a character like any other: such a pane (`sh -c 'make && ls'`)
starts wrongly or not at all, and so does one whose command was longer than
`status` keeps. The panes the default keys start (a
shell, by `keys/.shell`) and programs with plain arguments start as they
were. A project that needs its panes back exactly keeps a start script of
its own, which is a few lines of `fux start`.
For that to be of use after a reboot, the session directory must outlast
one. The default is usually not such a one on Linux ("Linux and macOS", below: it is one where `XDG_RUNTIME_DIR` is not set, `~/.cache/fux`); set `FUX_DIR` to a directory on disk, in your shell's
profile so that every `fux` agrees: `export FUX_DIR=~/.local/state/fux`.
On disk a busy pane costs writes (see "Bounds"). Then run the script at
login, before the first `fux view`. (`keys/.empty` is no place for it: that script runs only in a
workspace with no pane folders, and the panes to bring back are folders.)

## Don't

fux adds nothing to stop these. What happens is what Unix does:

- **Don't `rm -r` a pane you want.** The directory is the pane: its holder
  notices within 2 seconds, hangs up the program and leaves. That is also how
  to close one. `rm -r` of a workspace ends all its panes; a viewer that
  shows it says that the folder is gone and stays, until you show it another
  (`C-b w g`) or a new tab (`C-b t n`) makes the folder again.
- **Don't `mv` a live pane across filesystems** (that is a copy and an `rm`).
  Within one, the pane simply has the new name; the program's `FUX_PANE`
  still says the old one, so a default shell pane that was moved no longer
  removes its folder when its shell ends (it removes a folder only if it is
  still its own: it looks, and then removes by the name, so a pane moved to that name between the two is removed).
- **Don't kill the viewer** (a signal that ends it: it handles none; `C-b d`
  leaves it), **or type `reset` if you do.** Killed, it cannot put your
  terminal back as it found it (the main screen, the mouse, the keyboard
  flags): the terminal stays raw; with a program's keyboard flags left on,
  Ctrl-C reaches your shell as an escape sequence; and the cursor keeps the
  colour a pane gave it. A FIFO `.view.PID` stays in the session directory
  (`rm` it). The panes do not care.
- **Don't kill a holder with `kill -9`, or expect tidiness if you do.** The
  kernel hangs up its program. `state` stays as it was (`running`, as a rule) and the
  FIFOs stay, with no reader: the viewer says `lost` in the pane's corner, and a plain
  `printf > in` then waits for ever, as on any FIFO nobody reads (neither, while something of yours still holds `in` open for reading). `fux start`
  with the same name starts it again in place, with FIFOs of its own: who
  still waits on the old ones waits on. (A holder that ends normally
  removes its FIFOs, so a late `echo kill > ctl` only leaves a small file.)
- **Don't count on `kill`, or `rm -r`, killing.** They hang up the terminal.
  A program that ignores SIGHUP lives on (after `kill`, with `state hungup`,
  and what is then written to `in` goes nowhere), and so does anything it
  started in a session of its own. Its number is `pid` in `status`.
- **Don't expect a start that failed to tidy up.** `fux start` makes a
  folder and never removes one: a program that could not be run leaves its
  pane's folder with a `log` that says why. Start the name again, or `rm -r` it. A program that
  removes its pane's folder before the holder's first screen fails its start
  too (`the holder ended without a word`: the log went with the folder).
- **Don't remove a live pane's `in`**: that is how its holder knows the
  directory is gone, and it leaves.
- **Don't expect a pane to drop what its starter had open.** Past 0, 1 and 2, its
  terminal, it inherits the open files of whoever ran `fux start` (`3>&-` closes one).
- **Don't start a pane from a program that blocks signals.** A pane's program gets the
  signals its starter had blocked, blocked: Ctrl-C then does nothing there.
- **Don't start a pane again in the instant something reads it with `fux wait`.** `fux start`
  tries the pane's lock once: it then says a holder keeps the pane. Start it again.
- **Don't write more to `in` in one go than a FIFO takes whole, if others
  write too**: a larger write can interleave with theirs (how much that is:
  "Linux and macOS", below).
- **Don't run the viewer in a pane of the workspace it shows.** It draws its
  own pane as one of its tiles and asks it to be that small. In a pane of
  another workspace or session it is fine.
- **Don't move a pane into a workspace a viewer shows and expect silence.**
  A notice that appears there is a new one, whoever brought it: the pane's
  last notice is passed on once more (not one this viewer has already seen under that name: a pane moved away and back with the same notice is silent).
- **Don't expect one shell from two viewers that find a workspace empty at
  once.** Each starts one, as each would on its own: close one.
- **Don't put other folders in a workspace**: every folder there with a
  pane's kind of name is shown as a pane. Files are fine. And don't make a workspace folder by hand with `|`, `&` or `\` in its name (fux makes none): the search key's list is garbled by it.
- **Don't make `SHELL` a script whose interpreter is gone**: the file is there to run, so the default keys take the status of the start that failed for the shell's own, and the pane closes before the error can be read (`fux start -- "$SHELL"` says it and leaves it in the pane's `log`).
- **Don't take one failed `rm -r` of a busy pane's folder for a refusal**: its holder may write a file in the moment the folder is emptied (`Directory not empty`). Once more removes it.
- **Don't look for the `-` mark after choosing the tab already shown with `C-b f t`**: the list was a tab of its own and the one shown before it, and it is gone.
- **Don't edit `layout` by hand just as panes come or go**: the viewer writes
  the file then too, and one of the two edits is lost. `fux layout` takes a
  lock and loses none.
- **Don't write nonsense to `layout`** (or more than 160 KiB): the viewer says what is wrong in the corner
  of its last row and shows every pane as a tab, and `fux layout` refuses, until it is fixed.

## Linux and macOS

Differences are left as they are:

- The default session directory is usually in memory on Linux
  (`$XDG_RUNTIME_DIR/fux`) and on disk on macOS, which sets no such
  variable (`~/.cache/fux`): a flood there replaces small files on your
  disk as often as Bounds says. Point `FUX_DIR` at a RAM disk if that
  matters.
- One write of up to 4096 bytes to a FIFO arrives whole on Linux; 512 on macOS.
- macOS ships no `flock(1)`; `fux wait` takes the same lock, and waits for it.
- macOS file names ignore case by default, hence `S-h` for H. A pane named `LAYOUT` there is the workspace's `layout`: don't.
- macOS is covered by CI only (the tests, the viewer among them, and the
  example); nobody has used it there by hand.

## Coming from tmux

`ls -d "$(fux dir)"/*/` lists the workspaces (tmux's sessions); `fux view NAME`
attaches to one and `C-b d` detaches; a workspace with no pane gets a shell
each time a viewer comes to show it (`keys/.empty` does that, and is yours to change; with 32 key scripts still running it is not started, the corner says so, and it is not tried again until the workspace is shown anew; a
keys directory an earlier build wrote has none, and nothing is started), and
`C-b t n`, in the keys as they come, starts one in a workspace whose last
pane has ended. Tabs are tmux's windows. The choosers are `C-b f p`, `f t` and
`f w` (a pane, a tab, a workspace); there is no "last tab" key: it would be a script.

## Whose rules it follows

Terminal standards leave much open and terminals fill it differently, so
each kind of question has one authority here, and every place where fux
does something else is a row below with a case that shows it.

| Question | Authority |
| --- | --- |
| What a control sequence does | xterm. fux implements what the `xterm-256color` terminal description offers a program (`infocmp -x xterm-256color`) and, beyond it, only: line feed and index forms (`ESC D`, `CSI E`, `F`, `a`, `e`, `` ` ``, `I`), `CSI s` and `u` and mode 1048 (save and restore the cursor), origin mode (`CSI ? 6 h`), the alternate screens 47 and 1047, `ESC # 8`, the second character set (`ESC )` with SO and SI; a character set is chosen also with more bytes between the `ESC (` or `ESC )` and its letter, as `ESC ( ! ! 0`), SGR 6 (kept as 5), 9, 21, 58 and 59 and the colon forms of 4 (the kinds of underline), 38, 48 and 58, modes 1004 (focus) and 2026 (drawing), OSC 7, 8, 9, 777, 99 and 133, the mouse modes 9, 1002 and 1003, the questions `CSI 5 n`, `CSI ? N $ p` and `CSI 14`, `16` and `18 t`, `CSI 22 t` and `23 t` for the title, OSC 0 and 2, OSC 10 and 11 with a value and OSC 110 and 111 (a pane's own foreground and background; the description has the palette's and the cursor's, OSC 4, 12, 104 and 112) |
| Where one character ends and the next begins | Unicode's UAX #29 (grapheme clusters) and its test file, `GraphemeBreakTest.txt`, of Unicode 17. Always: a program that asks about mode 2027 (`CSI ? 2027 $ p`) is told it is set for good. A character goes on while the cursor is where its last part left it and its cell is unchanged: a change of colour or a title between a letter and its accent does not part them (`grep --color` does this), an erase does, and a move that leaves the cursor elsewhere (one away and back again parts nothing: `a`, CR, cursor right, U+0301 is `á`) |
| How many columns a character takes | One rule. A character (a cluster) takes the columns of its parts added up, two at most and one at least. A part adds two if it is East Asian wide or fullwidth or has emoji presentation (half a flag adds one, so a flag is two); none if it is a mark that takes no room (categories Mn and Me, the wide voiced marks of kana among them), a joiner, or the vowel or final consonant that follows in a Hangul syllable; one otherwise: the "ambiguous" ones, a mark that takes room (a vowel sign, a halfwidth voiced mark), a format character that starts a cluster (U+0600). An emoji with U+FE0F in its character is two. A control, U+2028 and U+2029 among them, and a mark that takes no room or a joiner with no character to join, takes no cell and is dropped |
| A newer protocol (sixel, OSC 52, bracketed paste, SGR mouse reports, the kitty keyboard protocol) | Its own specification |
| What a colour is (OSC 10, 11, 12, and 4 with `?`) | What the pane's program set (OSC 4, 10, 11, 12 with a value, until 104, 110 to 112 or a full reset gives it back); else the session's `colours` file: what the terminal you look at said, or what you wrote. No file or no line, no answer |

`tools/referee/referee.py` plays the same bytes to fux and to three
witnesses, and compares the screen's text and the cursor: xterm (under Xvfb,
never on your display), tmux (on a socket of its own) and the emulator of
the fux this one replaces. Its cases are files in `tools/referee/corpus/`:
every output capability of the terminal description (`terminfo/`), control
sequences where terminals are known to differ (`controls/`), Unicode strings
(`unicode/`), resizes (`resize/`) and recorded sessions of bash, vim, less
and htop (`sessions/`). xterm is the authority for control sequences only;
tmux and the old emulator are there because a difference from them is worth
a look. It sees text and cursor only: a case for a colour, a mode, an answer
or the bell shows that nothing else moved, and the rows below are the
departures it can see. The others are under "Left out". CI runs it on Linux and fails on
any difference that is not a row here, on a row that says the difference is
elsewhere (`text`, `cursor`), and on a case a witness could not be played.
Witnesses differ between their own versions (the rows were made with CI's
xterm and tmux and with newer ones; the referee prints which it ran): a row
stays while any of them differs, and the referee notes the rows that do not
differ from the ones you have. To replay a row: `cargo build --release &&
cargo build --release --manifest-path tools/referee/old/Cargo.toml &&
tools/referee/referee.py -v CASE` (`--witness tmux` if you have only tmux).
It needs `xterm`, `Xvfb` and `tmux`; its files go under `$TMPDIR` (else
`$XDG_RUNTIME_DIR`, else `~/.cache`) but for one empty file that stays, `~/.cache/fux-referee-display-N.lock`, for each display number it took, and Xvfb, as every X server, puts its socket in
`/tmp/.X11-unix`: under a number from 90 to 99 that the referee asks for and
that has no socket or lock there, never one the server picks (a server that
takes a desktop's number removes the desktop's socket when it ends).

<!-- referee:departures -->
| Cases | Differs from | In | What fux does, and why |
| --- | --- | --- | --- |
| `terminfo/smglp`, `terminfo/smglr`, `terminfo/smgrp` | old, xterm | text, cursor | No left and right margins: `CSI ? 69 h` is ignored and `CSI s` always saves the cursor. Nothing in the corpus's sessions sets them |
| `terminfo/mc5+mc4` | xterm | text, cursor | No printer: the printer codes are ignored and the text between them is shown; xterm sends that text to its printer |
| `terminfo/meml+memu` | xterm | text | No memory lock (`ESC l`, `ESC m`): xterm keeps the rows above the cursor from scrolling |
| `controls/newline-mode` | old, xterm | text, cursor | No line feed/new line mode (`CSI 20 h`): a line feed never returns to the first column |
| `controls/sub-in-sequence` | old, xterm | text, cursor | SUB ends a sequence and shows nothing; xterm shows an error character in its place |
| `controls/text-in-sequence` | xterm | text, cursor | A non-ASCII character inside a control sequence ends the sequence, and its last byte shows as U+FFFD; xterm takes `é` for the `i` it resembles in Latin-1's other half, which ends the sequence |
| `controls/text-in-sequence` | old, tmux | text, cursor | The same case: old and tmux drop the character and go on with the sequence |
| `controls/back-tab` | xterm | text, cursor | Back-tab (`CSI Z`) moves the cursor, and so ends a wrap that was waiting at the row's end, as every other move does (DEC STD 070); xterm leaves it waiting and the next character lands on the next row |
| `controls/bad-utf8`, `controls/overlong-utf8` | xterm | text | Bytes that are no character show one U+FFFD for each sequence, in the cells xterm uses too (the cursors agree); xterm's print of its screen writes them otherwise |
| `controls/overlong-utf8` | old | text, cursor | The same bytes: old and tmux count the marks otherwise, and tmux a waiting wrap (below) |
| `controls/bad-utf8` | tmux | cursor | The same bytes: old and tmux count the marks otherwise, and tmux a waiting wrap (below) |
| `controls/overlong-utf8` | tmux | text | The same bytes: old and tmux count the marks otherwise, and tmux a waiting wrap (below) |
| `controls/wide-in-one-column` | old, xterm | cursor | On a screen one column wide a wide character is dropped where the cursor is; xterm and old go to the next row first |
| `controls/wide-in-one-column-then-text` | tmux | text, cursor | The same on tmux, which then writes over the first character |
| `controls/wrap-off-then-on` | old, tmux | text, cursor | A wrap waits at the row's end even while wrapping is off, and is done when wrapping is turned on again, as in xterm; old and tmux forget it |
| `controls/ed-two-parameters` | old | text | `CSI 1 ; 0 J` erases as its first parameter says, as in xterm; old erases for each |
| `controls/wrap-pending-tab` | old | text, cursor | A tab at the row's end leaves the wrap waiting, as in xterm; old ends it |
| `controls/tab-stops` | old | text, cursor | `CSI 2 g` clears no tab stop, as in xterm; old clears all |
| `controls/back-tab`, `controls/tab-stops` | tmux | text | tmux's text of its screen has a tab character where a tab moved the cursor; fux has blanks |
| `controls/line-drawing-underscore`, `terminfo/acsc`, `terminfo/smacs+text` | tmux | text | tmux's text of its screen gives line-drawing characters as the letters that were sent |
| `controls/delete-lines-in-region` | tmux | text | Inserting or deleting lines puts the cursor in the first column, as in xterm; tmux leaves its column |
| `terminfo/dl`, `terminfo/dl1`, `terminfo/il`, `terminfo/il1` | tmux | text, cursor | Inserting or deleting lines puts the cursor in the first column, as in xterm; tmux leaves its column |
| `terminfo/is2`, `terminfo/rs2` | tmux | text, cursor | Leaving 132-column mode (`CSI ? 3 l`) does nothing, as in xterm by default; tmux clears the screen |
| `terminfo/rmcup` | tmux | text, cursor | Leaving the alternate screen when not on it still restores the saved cursor, as in xterm; tmux does nothing |
| `controls/alternate-1049-twice` | tmux | text | Asking for the alternate screen of 1049 while on it clears it again, as in xterm; tmux keeps it |
| `controls/alternate-47`, `controls/wrap-pending-cuf`, `controls/wrap-pending-report`, `resize/narrow-full-screen`, `unicode/mark-after-letter-at-end` | tmux | cursor | A cursor waiting to wrap is in the last column, reported and moved from there, as in xterm; tmux counts it one column further |
| `controls/wrap-pending-bs` | tmux | text, cursor | A cursor waiting to wrap is in the last column, reported and moved from there, as in xterm; tmux counts it one column further |
| `controls/save-restore-pen-and-wrap` | tmux | text, cursor | Saving and restoring the cursor keeps a waiting wrap, as in xterm; tmux forgets it |
| `controls/wrap-pending-el` | tmux | text, cursor | Erasing in the line ends a waiting wrap, as in xterm; tmux keeps it |
| `controls/line-position-relative` | tmux | text | `CSI e` and `CSI a` move down and right, as in xterm; tmux does not know them |
| `controls/wide-insert-inside` | tmux | text | Inserting in the middle of a two-column character blanks the whole character, as in xterm; tmux keeps it |
| `controls/c1-as-text` | tmux | text | A C1 control written as UTF-8 is dropped, as in xterm; tmux keeps it in its text |
| `controls/rep-after-mark` | tmux | text, cursor | REP after a letter with a combining mark repeats the letter, as in xterm; tmux repeats the mark |
| `resize/alternate-narrow` | tmux | cursor | Narrowing brings the cursor inside the screen; tmux reports it where it was |
| `unicode/devanagari-joiner`, `unicode/joiner-after-letter`, `unicode/joiner-after-letter-at-end`, `unicode/mark-after-move`, `unicode/three-marks`, `unicode/three-marks-at-end`, `unicode/vs15`, `unicode/vs15-at-end` | xterm | text | The cells and the cursor agree; xterm's print of its screen leaves out or reorders characters that take no column |
| `unicode/skin-tone`, `unicode/skin-tone-at-end` | xterm | cursor | xterm does not join characters into clusters and does not widen one for U+FE0F: each part takes its own columns, and at a row's end a cluster is cut where fux wraps it whole |
| `unicode/bengali-vowel-sign-at-end`, `unicode/devanagari-at-end`, `unicode/devanagari-joiner-at-end`, `unicode/family`, `unicode/family-at-end`, `unicode/family-four`, `unicode/family-four-at-end`, `unicode/flag-at-end`, `unicode/flags-three-at-end`, `unicode/halfwidth-voiced-mark-at-end`, `unicode/joiner-after-style`, `unicode/keycap`, `unicode/keycap-at-end`, `unicode/prepend-at-end`, `unicode/tamil-at-end`, `unicode/tamil-vowel-sign-at-end`, `unicode/thai-at-end`, `unicode/vs16`, `unicode/vs16-at-end` | xterm | text, cursor | xterm does not join characters into clusters and does not widen one for U+FE0F: each part takes its own columns, and at a row's end a cluster is cut where fux wraps it whole |
| `unicode/zero-width-space`, `unicode/zero-width-space-at-end` | old, tmux | text | A format character that joins nothing (U+200B) takes no cell and is dropped, as in xterm; old and tmux keep it in their text |
| `unicode/devanagari-joiner`, `unicode/flags-three`, `unicode/skin-tone`, `unicode/skin-tone-at-end` | tmux | cursor | tmux joins fewer characters than UAX #29 does (a spacing mark, a conjunct; before 3.8 also a flag, a skin tone, U+FE0F): what it does not join it measures apart and cuts at a row's end |
| `unicode/bengali-vowel-sign-at-end`, `unicode/devanagari-at-end`, `unicode/devanagari-joiner-at-end`, `unicode/flag-at-end`, `unicode/flags-three-at-end`, `unicode/halfwidth-voiced-mark-at-end`, `unicode/keycap-at-end`, `unicode/prepend-at-end`, `unicode/tamil-at-end`, `unicode/tamil-vowel-sign-at-end`, `unicode/thai-at-end`, `unicode/vs16-at-end` | tmux | text, cursor | tmux joins fewer characters than UAX #29 does (a spacing mark, a conjunct; before 3.8 also a flag, a skin tone, U+FE0F): what it does not join it measures apart and cuts at a row's end |
| `unicode/halfwidth-voiced-mark` | old | cursor | old measures the same cluster otherwise |
| `unicode/halfwidth-voiced-mark-at-end` | old | text, cursor | old measures the same cluster otherwise |
| `resize/region-then-resize` | old, tmux, xterm | text | When the text below the cursor no longer fits, rows go to `history` from the top, the cursor's row among them, and the cursor is kept at the top: no text is lost. The others keep the cursor's row and cut rows off the bottom (tmux: off the top) |
| `resize/cursor-above-text` | old, xterm | text | When the text below the cursor no longer fits, rows go to `history` from the top, the cursor's row among them, and the cursor is kept at the top: no text is lost. The others keep the cursor's row and cut rows off the bottom (tmux: off the top) |
| `resize/cursor-above-text` | tmux | text, cursor | When the text below the cursor no longer fits, rows go to `history` from the top, the cursor's row among them, and the cursor is kept at the top: no text is lost. The others keep the cursor's row and cut rows off the bottom (tmux: off the top) |
| `resize/taller` | old, tmux, xterm | text, cursor | What scrolled off into `history` stays there (it is a file other programs read): a screen made taller gets blank rows at its bottom, where the others bring rows back from their scrollback |
| `resize/overwrite-last-column` | tmux | cursor | After a line is wrapped anew the cursor is on the character it was on; tmux reports it at the line's end |
| `resize/alternate-leave-after`, `resize/hard-lines-narrow`, `resize/narrow`, `resize/narrow-cursor-mid-line`, `resize/narrow-wide-and-back`, `resize/narrow-wide-characters` | tmux | text, cursor | Narrowed, the screen keeps every row it has room for; tmux sends rows above the cursor to its history though the screen has room, and does not get them back when widened |
| `resize/narrow-full-screen` | xterm | text | The main screen's text is wrapped anew, as in tmux and the old emulator; xterm cuts rows at the right and loses what it cut |
| `resize/alternate-leave-after`, `resize/hard-lines-narrow`, `resize/narrow`, `resize/narrow-cursor-mid-line`, `resize/narrow-wide-characters`, `resize/overwrite-last-column`, `resize/widen` | xterm | text, cursor | The main screen's text is wrapped anew, as in tmux and the old emulator; xterm cuts rows at the right and loses what it cut |
<!-- referee:end -->

**Links.** A cell that is part of a link in its pane (the pane's `links`) is
drawn as part of one on your terminal (of 65,535 different addresses in one frame at most, those under a menu or a notice counted too: a cell of one more is drawn without its link), with the same address and no other
word of the program's: the link is closed before the next cell that is not
part of it and before the cursor is moved, so it ends at its tile's edge and
at the line between tiles, and a viewer that draws a link has closed it
before it writes anything else, and closes one when it leaves, and whatever
link was open on your terminal before it draws. A cell is drawn as its
own character with its own address: where your terminal would join it to the
one before it (two halves of a flag, each a link of its own), the viewer
keeps them apart with U+200B, as `screen.styled` does.
Opening a link is your terminal's doing. Without a viewer the `links` file
is there to read: `cut -d' ' -f4- "$FUX_PANE/shown/links"` in a key script lists
the addresses on the focused pane.


## Left out

A shell that does not mark its prompt, after its pane is made narrower: bash
draws a long command you have not entered yet a second time below the first
(it clears and redraws as if nothing had been wrapped anew; tmux shows the
same). Ctrl-L or Enter tidies it. With prompt marks (OSC 133: the bash lines
are with the `C-b C` key, above) it does not happen: while a command is
being typed, its line is not wrapped anew when the width changes (its rows
are cut or padded where they are, as kitty and Ghostty do), and the shell's
own redraw lands on it clean. That is done only where the line is surely the
shell's: its last two marks are a prompt and, after at least one cell of it, where the command begins (a prompt of no cells is not one), the
cursor is at or after that on its line, and the prompt began the line or stands above it. In every other case (output
that did not end its line with the prompt after it, a shell that marks only
its prompts, a command that runs) every line is wrapped anew as always, and
no text is ever cut that the shell would not draw again.


Of the mouse, the UTF-8 and urxvt report forms
(1005, 1015: programs get the SGR form or the oldest one), dragging a line
to resize, and any menu use;
a link's `id` (OSC 8: the address alone is kept and passed on) and links in history; reading the clipboard (setting it
works); a colour by its name (`OSC 4;1;red`) and the colours xterm has past the cursor's (OSC 13 to 19); the visual bell, the printer codes and memory lock; left and right margins; line feed mode (`CSI 20 h`);
more than ten
parts in one character (a letter with twelve marks loses the last three,
and the pane's `log` says so); prompts of the viewer's own (renaming a pane
would be a script); a copy mode (`C-b c` is the
editor on the pane's text, `C-b M-c` is `less` on it in colour); Windows. `clear` clears the screen, not the
`history` file (`CSI 3 J` is ignored).

Pictures: a pixel shape other than square (sixel can ask for pixels twice
as high as wide) is not kept: cells are counted as if pixels were square. Text
written over any cell of a picture takes the whole picture away, in the pane
and in the viewer (a sixel terminal keeps the pixels the text did not
cover): a pane's picture is one rectangle of cells, not pixels. A picture
keeps its place through a resize where the text beside it does: one on rows
that wrap anew at the new width goes; one past the right edge or the last
row of a smaller screen is kept, and shown again on a larger one (unless what is erased meanwhile reaches that edge: an erase to a row's end takes the pictures past it, one to the screen's end those below it); a resize
that tells of cells of another size in pixels takes every picture away.
After a picture
the cursor goes to the start of the line below it (a VT340 and xterm leave it on the
picture's last row), so that a prompt does not write over it; a picture
therefore needs a free row under it, and one as tall as the pane loses its
first rows off the top (it is drawn without them).

`TERM=xterm-256color` is the nearest honest name: the keys that are not the viewer's
reach the program as your terminal sends them, and what that terminal description
offers a program is implemented except for the items above.
