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
| `fux wait PANE-DIRECTORY` | waits until the pane's holder is gone, then prints `pid N` and `state …`: line 1 of its last screen, with a space where that has `=` or `:` |
| `fux view [WORKSPACE]` | the viewer |
| `fux layout [EDIT…]` | prints the workspace's layout, or edits it (below) |

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
| `ctl` | FIFO, one command a line: `kill` (hang up the program), `resize COLS ROWS [CELL-WIDTH CELL-HEIGHT]` |
| `screen` | line 1 says what rows cannot; then one line of plain text per row |
| `screen.styled` | the same with colours: text and SGR codes only, so `cat` shows it and nothing a program printed can reach your terminal as a control code |
| `status` | `holder N` and `command …` (its first 4,096 characters): written once, when the pane starts. The program's number and state are on line 1 of the screens, and only there |
| `history` | plain text of the lines that scrolled off the top. Past 4 MiB it becomes `history.1` and a new one starts |
| `pic-N.sixel` | a picture the program drew, whole: `cat` it in a sixel terminal |
| `title` | the window title the program set (OSC 0 or 2), one line, at most 256 characters, control characters taken out; rewritten only when it changes |
| `clipboard` | the bytes the program last put on the clipboard (OSC 52), at most 1 MiB, replaced in one step: `cat clipboard \| wl-copy`. A program that asks to read the clipboard is refused, with a line in `log` |
| `log` | what this holder refused or lost and why (a command it does not know, a picture over 4 MiB, history it could not write); stops at 64 KiB |

Line 1 of both screens:

```
pid=4242 state=running v=17 size=80x24 cursor=3,8 shape=0 modes=cursor,paste pic=pic-1.sixel@0,0+12x3
```

`v` grows by one with every change written (and would start again at 0); `cursor` is row,column from 0;
`pid` is the program's number and `state` what became of it: `running`,
`hungup`, `exited:CODE` or `killed:SIGNAL`;
`modes` lists what is on (`cursor` shown, `appcursor`, `keypad`, `paste`,
`mouse9`, `mouse1000`, `mouse1002` or `mouse1003`: the mouse reports the
program asked for, and `mousesgr`: their newer form,
`alt` screen); each `pic=` is a file, its row and column, and its size in
cells. A screen file is replaced in one step (written as `screen.new`, then
renamed), so a reader sees a whole old one or a whole new one.

**Bounds.** The screens are replaced at most 40 times a second, so a flood
(`yes`) costs at most 80 file replacements a second, and an append to
`history` with each pair (one more whenever 512 KiB of it wait); the holder reads at most 512 KiB of output between two of them, and
past that the program waits, as on any slow terminal. The holder keeps at most 64 KiB of typed bytes for a program that is not
reading; past that it stops reading `in`, and once the FIFO's own buffer
(the kernel's) is full, writers to `in` wait. A screen is at most 500 by 500
cells and keeps at most 8 pictures of at most 4 MiB of data each; a ninth drops the
oldest, with a line in `log`. History that cannot be written (a full disk)
is lost, with a line in `log`. When the program ends, at most 4 MiB of what
it wrote last is still read; a `resize` first draws, at the old size, up to
1 MiB of what the program had already written.

The program runs with `TERM=xterm-256color`, `TERM_PROGRAM=fux-next`,
`TERM_PROGRAM_VERSION` and `FUXN_PANE` (its pane's directory, whole path), in
the directory `fux start` was run in, with the rest of that environment and
the signals 1 to 31 at their defaults. Of this program's own `FUXN_…` names
it gets only `FUXN_PANE` and, if they were set, `FUXN_DIR` and `FUXN_KEYS`;
and neither `COLUMNS` nor `LINES`, which the holder reads for itself: the
pane's first size is `COLUMNS`x`LINES` if the starter set them (each 1 to
500; otherwise `log` says so), else 80x24.

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

The viewer asks a pane once to take the size of its tile (through `ctl`). Two
viewers with different sizes do not fight: the last to ask wins, and the
other shows what fits.

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
`COLUMNS` and `LINES` (the view's size, which a pane alone in a tab has), and this
`fux` first in `PATH`. Keys typed after it wait until it ends (two seconds
at most), so `C-b t n` and then text types into the new shell; a script that
takes longer puts its work in the background, as the defaults do.
What it prints goes to the viewer, a line at a time: `quit` leaves the
viewer, `workspace NAME` shows another workspace (`workspace -` the one this
viewer showed before, which is also where it goes when the one shown is
removed; failing that, `main`, if there is one), and anything else (an error
from `sh`, say) is shown in the bottom right corner until the next key. A key with no file, pressed in an
open menu, closes the menu and goes nowhere. Writing the same lines to the FIFO
`$FUXN_VIEW` does the same from anywhere.

The defaults are fux 1's keys, all under `keys/C-b/`: `v` split side by side,
`s` split stacked (both start in the directory the viewer was started in),
`x` then `y` close the pane (remove its directory), `h j k l` focus,
`o` next pane, `n` `b` next and previous tab, `d` detach, `c` the pane's text
in the editor, `C-b` send C-b itself; `t` tabs (`n` new, `h` `l`), `w`
workspaces (`n` new, `h` `l`), `r` resize (`h j k l`, repeating), `m` move
the pane (`h j k l`, repeating), `a` pane actions (`n` to a new tab, `k` send
TERM to the pane's program), `g` arrange (`r c g m`, `t` turn). Whether a pane
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
`nano`, `emacs` and `less` are opened at the file's end; any other editor
gets the file alone, as not every editor reads `+LINE`. The file is removed
when the editor ends. Its tab goes too (the
pane removes its own folder, as a shell pane does), and the
tab shown before it is shown again (the one you came from, unless you looked
at another meanwhile); an editor that failed first waits for Enter, so that
what it said can be read.

There is no zoom either: a pane shown alone is a tab with one pane. If you
want the key, these two scripts are it (`keys/C-b/z` and `keys/C-b/S-z`).
The first takes the focused pane to a tab of its own and keeps its old tab's
line in the pane's folder; the second writes that line back.

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
# Under the lock `fux layout` takes (flock on the workspace's folder), by way of perl, which Linux and macOS both have.
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
- **Don't kill a holder with `kill -9`, or expect tidiness if you do.** The
  kernel hangs up its program. Line 1 goes on saying `state=running` and the
  FIFOs stay, with no reader: the viewer says `lost` in the pane's corner, and a plain
  `printf > in` then waits for ever, as on any FIFO nobody reads. `fux start`
  with the same name starts it again in place. (A holder that ends normally
  removes its FIFOs, so a late `echo kill > ctl` only leaves a small file.)
- **Don't count on `kill`, or `rm -r`, killing.** They hang up the terminal.
  A program that ignores SIGHUP lives on (after `kill`, with `state hungup`,
  and what is then written to `in` goes nowhere), and so does anything it
  started in a session of its own. Its number is `pid=` on line 1 of `screen`.
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
empty one. Tabs are tmux's windows. There is no chooser and no "last tab"
key: both would be scripts.

## Whose rules it follows

Terminal standards leave much open and terminals fill it differently, so
each kind of question has one authority here, and every place where fux
does something else is a row below with a case that shows it.

| Question | Authority |
| --- | --- |
| What a control sequence does | xterm. fux implements what the `xterm-256color` terminal description offers a program (`infocmp -x xterm-256color`) and, beyond it, only: line feed and index forms (`ESC D`, `CSI E`, `F`, `a`, `e`, `` ` ``, `I`), `CSI s` and `u` and mode 1048 (save and restore the cursor), origin mode (`CSI ? 6 h`), the alternate screens 47 and 1047, `ESC # 8`, the second character set (`ESC )` with SO and SI), SGR 9, 21 and 58 and the colon forms of 38 and 48, the questions `CSI 5 n`, `CSI ? N $ p` and `CSI 14`, `16` and `18 t`, `CSI 22 t` and `23 t` for the title, OSC 0 and 2 |
| Where one character ends and the next begins | Unicode's UAX #29 (grapheme clusters) and its test file, `GraphemeBreakTest.txt` |
| How many columns a character takes | One rule: two for East Asian wide and fullwidth characters; none for combining marks that take no space (categories Mn and Me) and format characters from U+0300 on, the jamo U+1160 to U+11FF that complete a Hangul syllable, and emoji skin tones; one for everything else, the "ambiguous" ones too |
| A newer protocol (sixel, OSC 52, bracketed paste, SGR mouse reports) | Its own specification |

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
or the bell shows that nothing else moved. CI runs it on Linux and fails on
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
`/tmp/.X11-unix` under a number from 90 up.

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
| `controls/alternate-47`, `controls/wrap-pending-cuf`, `controls/wrap-pending-report` | tmux | cursor | A cursor waiting to wrap is in the last column, reported and moved from there, as in xterm; tmux counts it one column further |
| `controls/wrap-pending-bs` | tmux | text, cursor | A cursor waiting to wrap is in the last column, reported and moved from there, as in xterm; tmux counts it one column further |
| `controls/save-restore-pen-and-wrap` | tmux | text, cursor | Saving and restoring the cursor keeps a waiting wrap, as in xterm; tmux forgets it |
| `controls/wrap-pending-el` | tmux | text, cursor | Erasing in the line ends a waiting wrap, as in xterm; tmux keeps it |
| `controls/line-position-relative` | tmux | text | `CSI e` and `CSI a` move down and right, as in xterm; tmux does not know them |
| `controls/wide-insert-inside` | tmux | text | Inserting in the middle of a two-column character blanks the whole character, as in xterm; tmux keeps it |
| `controls/c1-as-text` | tmux | text | A C1 control written as UTF-8 is dropped, as in xterm; tmux keeps it in its text |
| `controls/rep-after-mark` | tmux | text, cursor | REP after a letter with a combining mark repeats the letter, as in xterm; tmux repeats the mark |
| `resize/alternate-narrow` | tmux | cursor | Narrowing brings the cursor inside the screen; tmux reports it where it was |
<!-- referee:end -->

Not done yet, and listed so the referee tells a new difference from these:
characters are not yet cut as UAX #29 says (a cell is one character and up to
two marks; emoji joined by U+200D, flags, keycaps and VS16 take the columns
of their parts), and rows are cut on a resize instead of wrapped again.

<!-- referee:departures -->
| Cases | Differs from | In | |
| --- | --- | --- | --- |
| `unicode/devanagari-at-end`, `unicode/keycap-at-end`, `unicode/tamil-at-end`, `unicode/thai-at-end`, `unicode/vs16-at-end` | old | text, cursor | clusters |
| `unicode/family`, `unicode/family-at-end`, `unicode/family-four`, `unicode/family-four-at-end`, `unicode/keycap`, `unicode/vs16` | old, tmux | cursor | clusters |
| `unicode/three-marks`, `unicode/three-marks-at-end` | old, tmux | text | clusters |
| `unicode/devanagari-joiner-at-end`, `unicode/flag-at-end`, `unicode/flags-three-at-end` | old, tmux | text, cursor | clusters |
| `unicode/prepend` | old, tmux, xterm | cursor | clusters |
| `unicode/prepend-at-end` | old, tmux, xterm | text, cursor | clusters |
| `unicode/devanagari-joiner`, `unicode/mark-after-letter-at-end` | tmux | cursor | clusters |
| `unicode/keycap-at-end` | tmux, xterm | text | clusters |
| `unicode/skin-tone`, `unicode/skin-tone-at-end` | xterm | cursor | clusters |
| `unicode/devanagari-joiner`, `unicode/devanagari-joiner-at-end`, `unicode/family`, `unicode/family-at-end`, `unicode/family-four`, `unicode/family-four-at-end`, `unicode/joiner-after-letter`, `unicode/joiner-after-letter-at-end`, `unicode/keycap`, `unicode/mark-after-move`, `unicode/vs15`, `unicode/vs15-at-end`, `unicode/vs16`, `unicode/vs16-at-end`, `unicode/zero-width-space`, `unicode/zero-width-space-at-end` | xterm | text | clusters |
| `resize/narrow-full-screen` | old | text | no reflow |
| `resize/alternate-leave-after`, `resize/hard-lines-narrow`, `resize/narrow` | old | text, cursor | no reflow |
| `resize/narrow-cursor-mid-line`, `resize/narrow-wide-characters`, `resize/widen` | old, tmux | text, cursor | no reflow |
| `resize/narrow-and-back`, `resize/region-then-resize` | old, tmux, xterm | text | no reflow |
| `resize/shorter-and-back`, `resize/taller` | old, tmux, xterm | text, cursor | no reflow |
| `resize/narrow-wide-and-back` | old, xterm | text, cursor | no reflow |
| `resize/alternate-leave-after`, `resize/hard-lines-narrow`, `resize/narrow`, `resize/narrow-wide-and-back` | tmux | text | no reflow |
| `resize/narrow-full-screen` | tmux | text, cursor | no reflow |
| `resize/narrow-wide-characters` | xterm | text | no reflow |
<!-- referee:end -->

## Left out

The kitty keyboard protocol; of the mouse, the UTF-8 and urxvt report forms
(1005, 1015: programs get the SGR form or the oldest one), dragging a line
to resize, and any menu use;
hyperlinks (OSC 8 is read and dropped); reading the clipboard (setting it
works); colour queries and palette and cursor colour changes (OSC 4, 10, 11,
12) and the queries DECRQSS, XTGETTCAP and XTVERSION; focus reports (mode
1004); the bell, the visual bell, the printer codes and memory lock; reflow on resize (rows keep their text, what does not fit is
cut or goes to history); left and right margins; line feed mode (`CSI 20 h`);
double underline and
underline colours (drawn as one plain underline); more than two combining
marks on a character; choosers and prompts (renaming
and picking from a list would be scripts); a copy mode (`C-b c` is the
editor on the pane's text); Windows. `clear` clears the screen, not the
`history` file (`CSI 3 J` is ignored).

Pictures: one that does not fit its tile whole is not drawn. After a picture
the cursor goes to the start of the line below it (xterm leaves it on the
picture's last row), so that a prompt does not write over it; a picture
therefore needs a free row under it, and one as tall as the pane scrolls off.

`TERM=xterm-256color` is the nearest honest name: keys reach the program
exactly as your terminal sends them, and what that terminal description
offers a program is implemented except for the items above.
