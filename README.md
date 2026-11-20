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

One package, no dependencies: the system calls, the terminal emulator, the
character widths and sixel are all here, in about 3,900 lines of Rust.
Linux and macOS.

```sh
cargo build --release                       # no crates fetched: Cargo.lock lists this package only
export PATH="$PWD/target/release:$PATH"     # so that `fux` below is this one
D=$(fux dir); fux start build -- sh       # a pane is a directory kept by one small process
printf 'echo hi\n' > "$D/main/build/in"; cat "$D/main/build/screen"     # plain tools are the clients
ls "$D" "$D/main"                           # workspaces are folders, panes are folders in them
fux view                                   # tiles, tabs, workspaces; vim, less and htop work in a pane
# a key is a script: C-b then `v` runs an executable file you can read and replace
cat ~/.config/fux-next/keys/C-b/v         # or under $FUXN_KEYS or $XDG_CONFIG_HOME, if you set them
echo kill > "$D/main/build/ctl"             # hangs up the program; what Unix does next is what happens
```

`./example.sh` runs the first lines for real; CI runs it on both systems.
Two notes on it: the screen is written a few milliseconds after the program
prints, so a script looks again until it sees what it waits for; and the key
scripts are written on the first `fux view`, to `$FUXN_KEYS`, else
`$XDG_CONFIG_HOME/fux-next/keys`, else `~/.config/fux-next/keys`.

## While fux 1 is the one you use daily

This is the fux that is to replace fux 1, on the branch `next`. Until fux 1
is retired its names are temporary, so that the two never meet: the
variables are `FUXN_…` (to become `FUX_…`), the session is
`$FUXN_DIR`, else `$XDG_RUNTIME_DIR/fux-next`, else `~/.cache/fux-next`, the
keys are `$FUXN_KEYS`, else `$XDG_CONFIG_HOME/fux-next/keys`, else
`~/.config/fux-next/keys`, and a pane's program sees `TERM_PROGRAM=fux-next`.
Do not `cargo install` it meanwhile: `fux` on your `PATH` stays fux 1, and
this one is run by its path (`target/release/fux`), as the example does with
`PATH` for one shell.

## Commands

| | |
|---|---|
| `fux dir` | prints the session directory |
| `fux start [NAME] [-- PROGRAM…]` | starts a pane and prints its directory. NAME is `PANE` (in workspace `main`, or typed in a pane, that pane's), `WORKSPACE/PANE`, or `WORKSPACE/` for the first free number. No program: `$SHELL`. A pane whose holder is gone is started again in place |
| `fux wait PANE-DIRECTORY` | waits until the pane's holder is gone, then prints `pid N` and `state …` (from `status` and `state`). When it returns, every file of the pane is the last there will be: wait with this, not by watching `state`, before you read a program's last screen |
| `fux view [WORKSPACE]` | the viewer |
| `fux layout [EDIT…]` | prints the workspace's layout, or edits it (below) |
| `fux help agents` | prints one page for a program (an agent, a script) that drives panes and has nothing else to read: how to start a pane, type into it (also so that a write cannot hang on a dead pane), read it, wait, get a command's output and exit code, tell a live pane from a dead one, rename (all in plain sh, with the risks said; exact forms for those who have perl in a section of their own), end and clean up, see what the viewer shows, with the manners and the hazards. It is `src/help-agents.txt`, in the binary; every command on it is run, in order, by `tools/help-agents.sh` (CI, both systems) |

Names are ASCII letters, digits, `.`, `_` and `-`, at most 64, not starting
with a dot, and not `-` alone; a pane cannot be called `layout` or
`layout.new`, in any case of letters.

Typed in a pane, a command means "here": with no workspace named, it is the
workspace that pane is in (and that pane's session, unless `FUXN_DIR` says
otherwise). Elsewhere it is `main`. So the example above, typed in a pane of
another workspace, starts `build` there and not in `main`. A key script is told the workspace shown
in `FUXN_WS`.

## A pane's files

| File | What it is |
|---|---|
| `in` | FIFO. Bytes written here are typed to the program |
| `ctl` | FIFO, one command a line: `kill` (hang up the program), `resize COLS ROWS [CELL-WIDTH CELL-HEIGHT]`, `notify TEXT` (have something noticed, as a program does: `printf 'notify %s\n' 'the build is done' > ctl`) |
| `screen` | the screen as plain text: one line per row, and nothing else |
| `screen.styled` | the same with colours: text and SGR codes only, so `cat` shows it and nothing a program printed can reach your terminal as a control code. Where two neighbouring characters would read as one (two halves of a flag a program put side by side), U+200B stands between them |
| `size` | `COLS ROWS` (`read cols rows < size`) |
| `cursor` | `ROW COL shown\|hidden SHAPE`: row and column from 0, and the shape's number as a program sets it (0 to 6, as a rule) |
| `modes` | one line of words, empty when none: how input is to be sent to the program. `appcursor`, `keypad`, `paste`, `focus` (it asked to be told when its pane gains and loses the keys), `alt` (the alternate screen), `mouse9`, `mouse1000`, `mouse1002` or `mouse1003` (the mouse reports the program asked for), `mousesgr` (their newer form), `keysN` (the keyboard flags in force for it, below) |
| `state` | what became of the program: `running`, `hungup`, `exited CODE` or `killed SIGNAL` |
| `status` | `holder N`, `pid N` (the program's number) and `command …` (its first 4,096 characters): written once, when the pane starts |
| `top` | how many lines of the pane's text are before the screen's first row, counted since the pane began: the number, from 0, of the line that row is part of. (After `history` has become `history.1` twice, the oldest of them are in neither file) |
| `wraps` | the rows of `screen`, numbered from 0, that go on from the row above (the text wrapped into them): one line of numbers, empty when none. A row that goes on in the next keeps the blanks at its end in `screen` and in `screen.styled`, so joining the two gives the line as it was printed (a line feed onto a row that goes on does not end the line above, as in tmux: a shell moves down a wrapped command with one. A program that writes two lines over a wrapped one without erasing it leaves them one line): with it a script can copy a wrapped line whole (the files are read one after another: of a pane that is busy, such a copy is as good as that moment) |
| `marks` | where a shell that marks its prompts (OSC 133) put its marks, a line each: `LINE AT KIND`, KIND being `prompt`, `command` (where what is typed begins), `output` or `end` (with the command's exit code after it, if the shell gave one from 0 to 255). LINE is counted from the pane's first line, as `top` is, a line being what was printed up to its line break however many rows it takes: line `top` is the screen's first, a smaller number is in `history`, in `history.1` or older than both. AT is how many bytes of the line's text are before the mark, as `history` and `screen` (its rows joined by `wraps`) have that text: `cut -b` and `LC_ALL=C awk` cut there, and never inside a character. A mark stays on its text: when lines are scrolled, inserted, deleted or wrapped anew its numbers follow. What is erased takes its marks to where the erased part begins, in their order (so does a prompt drawn again higher up, for the marks after it): an output that was cleared away is a pair of marks at one place, and nothing. The alternate screen has no marks, and `marks` are the shell's while it is shown. The last thousand; empty when the shell marks nothing |
| `links` | the links on the screen (a program makes one with OSC 8, as `ls --hyperlink` does), a line each: `LINE COL LEN ADDRESS`. LINE is the line's number as in `marks`, COL how many cells from the start of the line's first row on the screen the link begins (a whole row for each row the line has above it there), LEN how many cells it has, in reading order (a row counts as many cells as `size` says, also where a wide character that did not fit left its last cell empty, which `screen` does not show; a wide character is two cells); a link that goes over a line break is a line for each line. ADDRESS is printable ASCII without blanks, 2,048 bytes at most: anything else is no link, an address with a control byte in it among them. A cell keeps its link when it scrolls on the screen and when the text is wrapped anew, and loses it when it is erased or written over; text in `history` has none. At most a thousand different addresses at a time (one more is no link, with a line in `log`), and a list of 256 KiB at most (the runs past that are left out); empty when there are none. The files are read one after another: a reader that reads `links` of one moment and `screen` of the next has, until the next change, a link on the text that was there before |
| `pictures` | a line `pic-N.sixel ROW COL COLS ROWS` for each picture on the screen: the file, where it is, its size in cells |
| `history` | plain text of the lines that scrolled off the top: whole lines, however the screen had wrapped them (the last may be unfinished: its rest is on the screen still). Past 4 MiB it becomes `history.1` and a new one starts, where a line ends (a line that never ends is cut at 8 MiB). When what the last line went on in is erased from the screen, the line is ended in `history` too |
| `pic-N.sixel` | a picture the program drew, whole: `cat` it in a sixel terminal |
| `title` | the window title the program set (OSC 0 or 2), one line, at most 256 characters, control characters taken out; rewritten only when it changes |
| `clipboard` | the bytes the program last put on the clipboard (OSC 52), at most 1 MiB, replaced in one step: `cat clipboard \| wl-copy`. A program that asks to read the clipboard is refused, with a line in `log` |
| `cwd` | the directory the program says it is in (OSC 7, which shells can be set to send at each prompt), one line. Not there until it says; removed when it names another machine (a shell at the far end of ssh). At most 4,096 bytes. A shell that does not send OSC 7 can write the file itself: `pwd > "$FUXN_PANE/cwd"` |
| `notice` | what was last to be noticed: a number that grows with each one, then the words, on one line (at most 1 KiB, control characters taken out); the number alone is the bell. From the program (BEL; OSC 9 with words, not with a number and `;` first, which is a progress bar's and its like; OSC 777; OSC 99 with plain words) or from anyone through `ctl`. The file is replaced each time: what is unread is each reader's own to remember |
| `log` | what this holder refused or lost and why (a command it does not know, a picture over 4 MiB, history it could not write); stops at 64 KiB |

**One fact, one file.** The screens are text and nothing else; everything
else about a pane is a small file of its own, written when it changes and
not otherwise, replaced in one step (written as `NAME.new`, then renamed),
so a reader sees a whole old one or a whole new one (`history` is added to
in place, and a picture's file is written before `pictures` names it). A
file that could not be written is tried again in the next round, and `log`
says so once. In each round of
writing the facts come first and the screens last, `screen.styled` last of
all, and the screens are written again whenever any fact was: to follow a
pane, watch `screen.styled` and read what you need when it changes.

Files read one after another are not one moment: a fact may be one round
(25 ms) ahead of a text read just before it, or further apart in a folder
copied while its pane was busy. So keep the cursor inside the rows you have,
and take the rows that are there over what `size` says. The viewer does.
When a pane is still, or ended, its files agree.

**Bounds.** While a program keeps writing, the screens are replaced at most 40 times a second, so a flood
(`yes`) costs at most 80 file replacements a second (what a program wrote before it stopped for a
millisecond is shown at once, though never within 4 ms of the last time), and an append to
`history` with each pair (one more whenever 512 KiB of it wait); the holder reads at most 512 KiB of output between two of them, and
past that the program waits, as on any slow terminal. The holder keeps at most 64 KiB of typed bytes for a program that is not
reading; past that it stops reading `in`, and once the FIFO's own buffer
(the kernel's) is full, writers to `in` wait. A screen is at most 500 by 500
cells. A program that says it is in the middle of drawing (mode 2026) is not
written out until it says it is done, or half a second has passed since it first said so with no end (saying it again does not start the half second again). A screen keeps at most 8 pictures of at most 4 MiB of data each; a ninth drops the
oldest, with a line in `log`. History that cannot be written (a full disk)
is lost, with a line in `log`. When the program ends, at most 4 MiB of what
it wrote last is still read; a `resize` first draws, at the old size, up to
1 MiB of what the program had already written.

**How fast.** `tools/perf.py` measures fux beside tmux on the same steps, each
in one 200x60 pane seen through its viewer in a pty the script owns: a flood,
a key until its echo, the CPU at rest, a program that repaints every cell,
and a key while the viewer's output is read slowly. It needs python3, takes
about a minute and a half on two cores, and leaves nothing behind:

    cargo build --release && tools/perf.py target/release/fux

On the machine this was written on (Linux, arm64, two cores of twelve, tmux
3.8), the median of each (least to most), before the changes it was written
for (at 21cf936b4) and after them:

| | fux before | fux after | tmux |
|---|---|---|---|
| `seq 1 2000000`, until its end is on the screen | 4.37 s (4.37 to 4.41) | 1.46 s (1.44 to 1.46) | 1.01 s (1.00 to 1.02) |
| A key until its echo, typed slowly | 9.5 ms (5.6 to 13.2) | 4.0 ms (1.8 to 4.2) | 0.15 ms (0.07 to 2.7) |
| A key until its echo, typed fast (10 ms apart) | 20.1 ms (6.3 to 33.5) | 4.5 ms (2.3 to 6.4) | 0.11 ms (0.03 to 1.7) |
| At rest: the viewer (tmux: the client) | 3.2% of a core (2.6 to 4.3) | 0.12% (0.11 to 0.13) | 0% |
| At rest: the holder (tmux: the server) | 0.001% of a core | 0.001% | 0% (0 to 0.04) |
| All 200x60 cells repainted: frames the program wrote | 704 a second (702 to 707) | 686 a second (672 to 689) | 954 a second (942 to 957) |
| Meanwhile: the viewer (tmux: the client) | 27% of a core (25 to 29) | 13% (13 to 14) | 0% |
| Meanwhile: the holder (tmux: the server) | 55% of a core (52 to 57) | 54% (53 to 56) | 99% |
| Read at 100 KiB a second: a key until the program reads it | 52 ms (0.2 to 264) | 0.19 ms (0.08 to 1.9) | 0.11 ms (0.03 to 0.15) |

(tmux's server writes to the terminal itself, so its client does nothing; and
fux's holder reads at most 512 KiB between two screens, which is what bounds
the frames a program can write. A key's four milliseconds are the holder
waiting a millisecond to see that the program is done, the viewer's look at
the files every millisecond after a key, and its putting the frame together.)

The program runs with `TERM=xterm-256color`, `TERM_PROGRAM=fux-next`,
`TERM_PROGRAM_VERSION`, `COLORTERM=truecolor` and `FUXN_PANE` (its pane's directory, whole path), in
the directory `fux start` was run in, with the rest of that environment and
the signals 1 to 31 at their defaults. Of this program's own `FUXN_…` names
it gets only `FUXN_PANE` and, if they were set, `FUXN_DIR` and `FUXN_KEYS`;
and neither `COLUMNS` nor `LINES`, which the holder reads for itself: the
pane's first size is `COLUMNS`x`LINES` if the starter set them (each 1 to
500; otherwise `log` says so), else 80x24.

What is this terminal, and what can it do? Programs ask, and fux answers as
itself and for itself, with nobody watching:

| Asked | Answered |
|---|---|
| `CSI > q` (XTVERSION) | `fux-next VERSION` |
| `CSI c`, `CSI > c` (device attributes) | `CSI ? 62;4;22 c` (a VT220 with sixel and colour) and `CSI > 70;0;0 c`: a kind of terminal no other gives (xterm is 41, tmux 84, Ghostty 1 with version 10), so nothing a program knows of another terminal's versions is turned on |
| `DCS $ q … ST` (DECRQSS) | what the emulator keeps: `m` (the colours and looks in force, in the form with colons), `r` (the scroll region), `SP q` (the cursor's shape); "not valid" for anything else |
| `DCS + q NAMES ST` (XTGETTCAP) | each name by itself, from the table below; "not valid" for every other name, so a name fux does not know never hides one it does (more than 32 names or 2,048 bytes in one question are one "not valid") |

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

That is all of it: true colour, the kinds and the colour of an underline, the
cursor's shape (`Se` gives it back to your terminal's own). Setting the
clipboard works (OSC 52 with `c`) and is not in the table: the capability's
name, `Ms`, promises a choice of clipboard and a way to read it, and fux has
neither. What is not there fux does not do
(left and right margins among them: `CSI ? 69 $ p` is answered "not known"),
and a program that asks first does not turn it on. A pane's program also has
`COLORTERM=truecolor` in its environment, which is what many read instead of
asking. `TERM` stays `xterm-256color`: fux has no terminal description of
its own to install, and claims no other terminal's. Three things follow.
The viewer passes 24-bit colours on as they are, so on a terminal that has
none of them (few are left) set `COLORTERM=` for the program yourself. A
program that goes by `TERM` alone may use what an xterm has and fux has not:
a nested tmux sets the cursor's colour (heard, without effect) and needs
`set -as terminal-features 'xterm-256color:usstyle'` to pass curly
underlines on. And an answer nobody reads is typed text, as on any terminal:
a program that asks and ends at once leaves it on its last screen, and one
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

(`foreground`, `background`, or a palette number from 0 to 255, then `#` and
six hex digits; of two lines for one colour the last counts, a line that is
anything else counts for nothing, and 16 KiB of the file are read). The
viewer asks its terminal once when it starts, gives it a second to answer
(and writes again if more comes within ten, on a slow line),
and writes the file with what it was told, in place of the one that was
there, yours too; a terminal that tells of no colour leaves the file as it
is. Two viewers on terminals of different colours do not agree: the one that
wrote last is believed. A pane's holder answers a program's question (OSC
10, 11, and 4 with `?`) from that file, with or without a viewer, and says
nothing about a colour the file has no line for: a program is never told a
colour that someone made up. A program that sets a colour (OSC 4, 10, 11
with a value, 104, 110, 111) is heard and changes nothing. No workspace or
pane can be named `colours`.

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
last is believed); `echo 5 > "$(fux dir)/keyboard"` writes it by hand. A
program is told, when it asks, what the file says then, and that stays true
for it: with no file, or `0`, it is told that no flag is in force and gets
keys the old way, also after a viewer on a better terminal has come (start
the program again to have them). While a pane has the keys, the viewer asks
its terminal for that pane's flags, and takes them back when the focus moves
or the viewer leaves. The keys go to the program as the terminal sends them.
A flag the terminal lacks is not in force, and the program is told so. (What
a program was told stays what `modes` says, also when a viewer on a lesser
terminal comes later: that terminal sends what it can, and the program
finds out when it asks again.)

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
key is `M-S-3`); the keypad's keys are named as the keys they double. Two
keys that the old form cannot tell apart have two names in the new one:
Ctrl-I is `C-i` and no longer `tab`, Ctrl-Backspace is `C-backspace` and no
longer `C-h`. Who has the keys is read from `layout` when keys come, so a
key typed right after `fux layout focus …` is the new pane's; until the
viewer has drawn once more, it may reach that pane in the form the last one
asked for. `keyboard` is
no name for a workspace or a pane either.

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
gets the keys. When the tab shown goes (its last pane ends), the `-` tab is
shown, as `cd -` would (a tab goes when its last pane's folder is removed,
not when a program merely ends); an edit that ends on another tab moves the mark. It
is in the file, so two viewers agree. A folder the file does not place gets a
tab of its own; a name with no folder is dropped. A part there is no room
for (more parts than cells) is hidden. The focused pane gets a cell even
then. A workspace shows at most 1,000 panes. `fux layout EDIT` makes
one edit:

`split row|col PANE [beside ANCHOR]` (put PANE beside or below ANCHOR, defaulting to the focused pane) ·
`focus left|right|up|down|next|prev|PANE` · `pane PANE` (the same, for a
pane named like a direction) · `tab next|prev|N|-` (`-`: the tab shown before) · `break`
(the focused pane to its own tab) · `swap next|prev` · `size row|col +N|-N` ·
`rotate` · `arrange row|col|grid|main`

Your terminal may not count a character's columns as fux does (it may draw
three people where fux joined a family). The viewer keeps that inside the
cell: after anything but plain ASCII it places the next cell by row and
column, it sends the cells such a character may have been drawn over again,
and where one could reach past its tile's edge it draws a `?` in each of its
columns instead (the pane's `screen` keeps the character): a family in the
last six columns of a tile is `??`, since a terminal that does not join it
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
held: `C-b`) until its pane has had the keys. Of two within a quarter of a
second the last is passed on. Another workspace's notices are not looked at. What was there when the viewer started only marks its tab. A program
that asked to be told about focus (mode 1004) is told when its pane gains
and loses the keys, and when your terminal's window does.

The viewer asks a pane to take the size of its tile (through `ctl`), once each time the tile changes. Two
viewers with different sizes do not fight: the last to ask wins, and the
other shows what fits.

A resize wraps the main screen's text anew: a line that was wrapped is one
line again at any width, and no character is cut in two (on a screen one
column wide, a wide character becomes a blank). Rows that no longer fit go
to `history` from the top, and come back from it at the next resize that has
room for them, if nothing has scrolled off in between: a tile made narrow
for a moment is as it was when it is wide again, and `history` is then that
much shorter at its end. What scrolled off stays in `history`: a screen made
taller later gets blank rows at its bottom. The cursor stays on its
character while that character's row is on the screen; if the text below it
needs the room, that row goes too and the cursor is kept at the top. The
alternate screen (vim, less) is cut or padded, never wrapped, and nothing of
it goes to history: its program draws it again. A size that is refused
changes nothing and leaves a line in `log`.

### Keys are scripts

The keys directory is `$FUXN_KEYS`, else `$XDG_CONFIG_HOME/fux-next/keys`, else
`~/.config/fux-next/keys`; the viewer
writes the default scripts there if it does not exist (and only then: after
an upgrade, remove the folder, or copy `keys/` from the source over it, to
get the new defaults). A key that has a file
of its name runs it (a file that cannot be run says so in the corner). A key that has a directory opens it as a
menu (each script's second line, `# what it does`, is its entry) and the next
key is looked up there. Any other key goes to the program. So the prefix is
only a directory: `keys/C-b/v` is "C-b, then v", and `keys/M-left` would be
Alt-Left with no prefix.

Key names: a character is itself (`v`, `%`, `é`); `S-h` is capital H (file
names differ by more than case, for macOS); `C-b`, `M-x`, `enter`, `tab`,
`esc`, `space`, `slash`, `dot`, `backspace`, `up` `down` `left` `right`,
`home` `end` `pgup` `pgdn` `insert` `delete`, `f1`…`f12`, with `C-`, `M-`,
`S-` before them when held (`C-left`, `S-f5`).

A script gets `FUXN_DIR` (the session), `FUXN_WS` (the workspace shown),
`FUXN_PANE` (the focused pane's directory), `FUXN_KEYS`, `FUXN_VIEW`,
`COLUMNS` and `LINES` (the view's size, which a pane alone in a tab has),
`FUXN_ROW` and `FUXN_COL` (the size, as `COLSxROWS`, of the tile a split of the
focused pane would make: `FUXN_ROW` side by side, as `C-b v` splits, `FUXN_COL`
one above the other, as `C-b s` does; the split keys
start their pane at that size, so its program is not drawn twice), `FUXN_SELF`
(the pane this viewer itself runs in, if it runs in one), and this
`fux` first in `PATH`. Keys typed after it wait until it ends (two seconds
at most), so `C-b t n` and then text types into the new shell; a script that
takes longer puts its work in the background, as the defaults do.
What it prints goes to the viewer, a line at a time: `quit` leaves the
viewer, `workspace NAME` shows another workspace (`workspace -` the one this
viewer showed before, which is also where it goes when the one shown is
removed; failing that, `main`, if there is one), and anything else (an error
from `sh`, say) is shown in the bottom right corner until the next key. A key with no file, pressed in an
open menu, closes the menu and goes nowhere (in a menu that stays open, as resize and move do, the prefix opens its own menu again: `C-b r l l C-b v` resizes and then splits). Writing the same lines to the FIFO
`$FUXN_VIEW` does the same from anywhere.

The defaults are fux 1's keys, all under `keys/C-b/`: `v` split side by side,
`s` split stacked (both, and a new tab, start where the focused pane's program says it is, its `cwd`, and else in the directory the viewer was started in),
`x` then `y` close the pane (remove its directory), `h j k l` focus,
`o` next pane, `n` `b` next and previous tab, `d` detach, `c` the pane's text
in the editor, `C` (Shift-c) the last command's output in the editor (from
where it began to where it ended, so without the prompt that
follows an output with no line break; it says so if the command printed
nothing or its output was cleared away; of a long output that Ctrl-L or `clear` cut in two,
the part that had scrolled into history is all there is, and it opens that) and `p` the editor at the prompt before this one, or at the only one
(both for a shell that marks its prompts, and neither while the pane shows a
program's own screen: for bash from 4.4 on, `PS1='\[\e]133;A\a\]'$PS1'\[\e]133;B\a\]'; PS0='\e]133;C\a';
PROMPT_COMMAND='printf "\e]133;D;%s\a" "$?"'`, the `printf` put first in a
`PROMPT_COMMAND` you have already), `C-b` send C-b itself; `t` tabs (`n` new, `h` `l`), `w`
workspaces (`n` new, `h` `l`), `r` resize (`h j k l`, repeating), `m` move
the pane (`h j k l`, repeating), `a` pane actions (`n` to a new tab, `k` send
TERM to the pane's program), `g` arrange (`r c g m`, `t` turn), `f` find
(`p` a pane, `t` a tab, `w` a workspace: a list in a pane of its own, chosen
with fzf, sk or fzy if one is installed and by number or name if none is (a number is a line of the list first, and a name only if the list has no such line); the
workspace that holds the viewer's own pane is not offered, since a viewer
that shows itself draws itself without end). Whether a pane
closes when its shell exits is the script's choice too: the defaults start
not your shell but `keys/.shell`, a few lines that run it and then remove
the pane's own folder, however the shell ended. Only when `.shell` itself
is gone first is nobody left to remove the folder, and it stays until the
close key or `rm -r` takes it: saying `killed` after `kill` in `ctl` or a
`.shell` killed outright, and `lost` when the holder was killed. The `a k`
key sends TERM to the pane's program, which in a default shell pane is
`.shell`: that ignores TERM, as an interactive shell does, so the key does
nothing there; it is for panes started with a program of their own. The close key moves the folder out of the
workspace (the tile goes at once), removes its `in`, and removes the folder
when the holder has left, within two seconds.

In a menu's directory, a file `.about` holds the words shown for it in the
menu above, and a file `.repeat` keeps the menu open after each key (until a
key it has not): that is all "resize mode" is. A directory with one script,
like `x/y`, is a question.

A script's third line may be `# group: NAME` (and so may the second line of
an `.about`): the menu lists keys under their group's name, groups in
alphabetical order, keys with no group first (the menu reads a script's
first kilobyte for its caption and group). A menu taller than the
terminal goes on in a second column.

There is no copy mode: `C-b c` puts the pane's older history, its history and
its screen in one private file and opens it in `$EDITOR` (which may hold
arguments, split at spaces), else `$PAGER`, else `less`, in a new tab. `vi`, `vim`, `nvim`,
`nano`, `emacs`, `less`, `hx` and `helix` are opened at the file's end (and by the prompt
keys at their line); any other editor
gets the file alone, as not every editor reads `+LINE`. An editor behind a
launcher of another name is one of those: say how yours is run in
`FUXN_EDIT`, a command in which `%f` is the file and `%l` the line, set where
the viewer is started (a viewer started in a pane has it too: `FUXN_EDIT` is passed on to a pane's program, as `FUXN_DIR` and `FUXN_KEYS` are; `export FUXN_EDIT='hx %f:%l'`, or your launcher in
`hx`'s place; `keys/.edit` reads it and is yours to change). The file is in the editor pane's own folder and is removed
when the editor ends, or with the pane if that is closed first. Its tab goes too (the
pane removes its own folder, as a shell pane does), and the
tab shown before it is shown again (the one you came from, unless you looked
at another meanwhile); an editor that failed first waits for Enter, so that
what it said can be read.

There is no zoom either: a pane shown alone is a tab with one pane. If you
want the key, these two scripts are it (`keys/C-b/z` and `keys/C-b/S-z`).
The first takes the focused pane to a tab of its own and keeps its old tab's
line in the pane's folder; the second writes that line back. (`fux layout`
is the way to change a layout that needs nothing else and loses no edit. The
second script edits the file by hand, which `fux layout` has no edit for, and
so takes the same lock itself: sh has no way to, and it uses perl for that
one line. Without perl, leave the line out: the edit is then as safe as any
hand edit, lost if a pane comes or goes in the same instant.)

```sh
#!/bin/sh
# show this pane alone, in a tab of its own
# The tab's line of the layout is kept in the pane's folder; `C-b Z` puts it back.
[ -n "$FUXN_PANE" ] || exit 0
[ -f "$FUXN_PANE/alone" ] && { echo "already alone: C-b Z puts it back"; exit; }
fux layout | grep '^\*' > "$FUXN_PANE/alone.new" || exit
fux layout break
# A pane that was alone already has not moved: there is nothing to put back.
if fux layout | grep '^\*' | cmp -s - "$FUXN_PANE/alone.new"
then rm -f "$FUXN_PANE/alone.new"
else mv "$FUXN_PANE/alone.new" "$FUXN_PANE/alone"
fi
```

```sh
#!/bin/sh
# put this pane back where `C-b z` took it from
# The layout is text: the kept line replaces the line of the tab the pane left,
# and the pane's own line goes.
[ -f "$FUXN_PANE/alone" ] || { echo "this pane was not taken out with C-b z"; exit; }
ws=$FUXN_DIR/$FUXN_WS me=${FUXN_PANE##*/}
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
' "$FUXN_PANE/alone" "$ws/layout" > "$ws/.layout.$$" && mv "$ws/.layout.$$" "$ws/layout" && rm -f "$FUXN_PANE/alone"
```

Put back: the tab, the pane's place in it, every share, the focus. Not put
back: a pane closed meanwhile (dropped), a pane opened meanwhile (it keeps
its own tab), and a share that another viewer or script changed in the old
tab while the pane was out, which the kept line overwrites. If the pane's
old neighbours are in different tabs by then, the kept line replaces the
first tab that holds one of them. Closing the pane
forgets the line; a pane named `row` or `col` confuses the match.

**The look** is fux 1's. A pane has every cell but the one-cell line between it
and its neighbour (`│`, `─`, a tee or a cross where lines meet); the lines
touching the focused pane are green. There are no titles and no bar: while a
prefix is in use, the last row shows the workspace, its tabs and the keys
typed so far, and the corner above it the keys that can follow. A message
(a script's words, what is wrong with `layout`) sits in the bottom right
corner until the next key. A pane whose program ended, or whose holder is
gone, says so in the corner of its own last row (`exited 0`, `lost`).

**Titles and the clipboard.** The viewer gives your terminal the focused
pane's title (and its own back when no pane has one, and when it leaves);
no row is taken for it. What the focused pane's program copies (OSC 52)
is passed to your terminal once, when its `clipboard` file changes: looking at a pane, or
coming back to it, never sends an old clipboard again. Without a viewer the
`clipboard` file is still there to read.

**The mouse** passes through. When the focused pane's program asks for mouse
reports, the viewer asks your terminal for the same and forwards each one to
that pane, counted from its tile's corner; a drag that leaves the tile is
reported at its edge until the button is let go. A click on another tile
gives that tile the keys and is not passed on. The viewer uses the mouse for nothing else, and when the
focused program does not ask, your terminal's own selection works as ever.

Pictures: when the terminal says it draws sixel, a pane's picture is drawn in
its tile if it fits whole. Another protocol later is another file extension
beside `.sixel` and a few lines in the viewer.

## Don't

fux adds nothing to stop these. What happens is what Unix does:

- **Don't `rm -r` a pane you want.** The directory is the pane: its holder
  notices within 2 seconds, hangs up the program and leaves. That is also how
  to close one. `rm -r` of a workspace ends all its panes.
- **Don't `mv` a live pane across filesystems** (that is a copy and an `rm`).
  Within one, the pane simply has the new name; the program's `FUXN_PANE`
  still says the old one, so a default shell pane that was moved no longer
  removes its folder when its shell ends (it removes a folder only if it is
  still its own).
- **Don't kill the viewer with a signal, or type `reset` if you do.** It
  puts your terminal back as it found it (the main screen, the mouse, the
  keyboard flags) when it leaves by itself; killed, it cannot, and with a
  program's keyboard flags left on, Ctrl-C reaches your shell as an escape
  sequence.
- **Don't kill a holder with `kill -9`, or expect tidiness if you do.** The
  kernel hangs up its program. `state` goes on saying `running` and the
  FIFOs stay, with no reader: the viewer says `lost` in the pane's corner, and a plain
  `printf > in` then waits for ever, as on any FIFO nobody reads. `fux start`
  with the same name starts it again in place. (A holder that ends normally
  removes its FIFOs, so a late `echo kill > ctl` only leaves a small file.)
- **Don't count on `kill`, or `rm -r`, killing.** They hang up the terminal.
  A program that ignores SIGHUP lives on (after `kill`, with `state hungup`,
  and what is then written to `in` goes nowhere), and so does anything it
  started in a session of its own. Its number is `pid` in `status`.
- **Don't remove a live pane's `in`**: that is how its holder knows the
  directory is gone, and it leaves.
- **Don't expect a pane to drop what its starter had open.** It inherits the
  open files of whoever ran `fux start`, as any child does (`3>&-` closes one).
- **Don't write more than 512 bytes to `in` in one go if others write too**:
  larger writes can interleave (4096 on Linux).
- **Don't kill the viewer** (with any signal: it handles none; `C-b d`
  leaves it). Your terminal stays raw (`reset`)
  and a FIFO `.view.PID` stays in the session directory (`rm` it). The panes
  do not care.
- **Don't run the viewer in a pane of the workspace it shows.** It draws its
  own pane as one of its tiles and asks it to be that small. In a pane of
  another workspace or session it is fine.
- **Don't put other folders in a workspace**: every folder there with a
  pane's kind of name is shown as a pane. Files are fine.
- **Don't edit `layout` by hand just as panes come or go**: the viewer writes
  the file then too, and one of the two edits is lost. `fux layout` takes a
  lock and loses none.
- **Don't write nonsense to `layout`**: the viewer says what is wrong in the corner
  of its last row and shows every pane as a tab until it is fixed.

## Linux and macOS

Differences are left as they are:

- The default session directory is usually in memory on Linux
  (`$XDG_RUNTIME_DIR/fux-next`) and on disk on macOS, which sets no such
  variable (`~/.cache/fux-next`): a flood there is up to 80 small file
  replacements a second on your disk. Point `FUXN_DIR` at a RAM disk if that
  matters.
- One write of up to 4096 bytes to a FIFO arrives whole on Linux; 512 on macOS.
- macOS ships no `flock(1)`; `fux wait` is the same test.
- macOS file names ignore case by default, hence `S-h` for H.
- macOS is covered by CI only (the tests, the viewer among them, and the
  example); nobody has used it there by hand.

## Coming from tmux

`ls "$(fux dir)"` lists the workspaces (tmux's sessions); `fux view NAME`
attaches to one and `C-b d` detaches; `C-b t n` starts the first shell in an
empty one. Tabs are tmux's windows. The choosers are `C-b f p`, `f t` and
`f w` (a pane, a tab, a workspace); there is no "last tab" key: it would be a script.

## Whose rules it follows

Terminal standards leave much open and terminals fill it differently, so
each kind of question has one authority here, and every place where fux
does something else is a row below with a case that shows it.

| Question | Authority |
| --- | --- |
| What a control sequence does | xterm. fux implements what the `xterm-256color` terminal description offers a program (`infocmp -x xterm-256color`) and, beyond it, only: line feed and index forms (`ESC D`, `CSI E`, `F`, `a`, `e`, `` ` ``, `I`), `CSI s` and `u` and mode 1048 (save and restore the cursor), origin mode (`CSI ? 6 h`), the alternate screens 47 and 1047, `ESC # 8`, the second character set (`ESC )` with SO and SI), SGR 6 (kept as 5), 9, 21, 58 and 59 and the colon forms of 4 (the kinds of underline), 38, 48 and 58, modes 1004 (focus) and 2026 (drawing), OSC 7, 8, 9, 777, 99 and 133, the mouse modes 9, 1002 and 1003, the questions `CSI 5 n`, `CSI ? N $ p` and `CSI 14`, `16` and `18 t`, `CSI 22 t` and `23 t` for the title, OSC 0 and 2 |
| Where one character ends and the next begins | Unicode's UAX #29 (grapheme clusters) and its test file, `GraphemeBreakTest.txt`, of Unicode 17. Always: a program that asks about mode 2027 (`CSI ? 2027 $ p`) is told it is set for good. A character goes on while the cursor is where its last part left it and its cell is unchanged: a change of colour or a title between a letter and its accent does not part them (`grep --color` does this), a move or an erase does |
| How many columns a character takes | One rule. A character (a cluster) takes the columns of its parts added up, two at most and one at least. A part adds two if it is East Asian wide or fullwidth or has emoji presentation (half a flag adds one, so a flag is two); none if it is a mark that takes no room (categories Mn and Me, the wide voiced marks of kana among them), a joiner, or the vowel or final consonant that follows in a Hangul syllable; one otherwise: the "ambiguous" ones, a mark that takes room (a vowel sign, a halfwidth voiced mark), a format character that starts a cluster (U+0600). An emoji with U+FE0F in its character is two. A control, U+2028 and U+2029 among them, and a mark or joiner with no character to join, takes no cell and is dropped |
| A newer protocol (sixel, OSC 52, bracketed paste, SGR mouse reports, the kitty keyboard protocol) | Its own specification |
| What a colour is (OSC 10, 11, and 4 with `?`) | The session's `colours` file: what the terminal you look at said, or what you wrote. No file or no line, no answer. Setting a colour (OSC 4, 10, 11 with a value, 104, 110, 111) is taken and changes nothing |

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
It needs `xterm`, `Xvfb` and `tmux`; its files go under `$TMPDIR` (or
`$XDG_RUNTIME_DIR`), and Xvfb, as every X server, puts its socket in
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
drawn as part of one on your terminal, with the same address and no other
word of the program's: the link is closed before the next cell that is not
part of it and before the cursor is moved, so it ends at its tile's edge and
at the line between tiles, and a viewer that draws a link has closed it
before it writes anything else, and closes one when it leaves, and whatever
link was open on your terminal before it draws. A cell is drawn as its
own character with its own address: where your terminal would join it to the
one before it (two halves of a flag, each a link of its own), the viewer
keeps them apart with U+200B, as `screen.styled` does.
Opening a link is your terminal's doing. Without a viewer the `links` file
is there to read: `cut -d' ' -f4- "$FUXN_PANE/links"` in a key script lists
the addresses on the focused pane.


## Left out

A shell's own redraw after its pane is made narrower: bash draws a long
command you have not entered yet a second time below the first (it clears
and redraws as if nothing had been wrapped anew; tmux shows the same). Ctrl-L
or Enter tidies it.


Of the mouse, the UTF-8 and urxvt report forms
(1005, 1015: programs get the SGR form or the oldest one), dragging a line
to resize, and any menu use;
a link's `id` (OSC 8: two runs with one address are two links to your terminal) and links in history; reading the clipboard (setting it
works); changing a colour (OSC 4, 10, 11, 12 with a value, 104, 110, 111, 112:
taken, without effect) and asking for the cursor's (OSC 12 with `?`); the visual bell, the printer codes and memory lock; left and right margins; line feed mode (`CSI 20 h`);
more than ten
parts in one character (a letter with twelve marks loses the last three,
and the pane's `log` says so); prompts (renaming
would be a script); a copy mode (`C-b c` is the
editor on the pane's text); Windows. `clear` clears the screen, not the
`history` file (`CSI 3 J` is ignored).

Pictures: one that does not fit its tile whole is not drawn. After a picture
the cursor goes to the start of the line below it (a VT340 and xterm leave it on the
picture's last row), so that a prompt does not write over it; a picture
therefore needs a free row under it, and one as tall as the pane scrolls off.

`TERM=xterm-256color` is the nearest honest name: keys reach the program
exactly as your terminal sends them, and what that terminal description
offers a program is implemented except for the items above.
