# fux3, from a clean slate

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
character widths and sixel are all here, in about 3,000 lines of Rust.
Linux and macOS.

```sh
cargo build --release                       # no crates fetched: Cargo.lock lists this package only
export PATH="$PWD/target/release:$PATH"     # so that `fux3` below is this one
D=$(fux3 dir); fux3 start build -- sh       # a pane is a directory kept by one small process
printf 'echo hi\n' > "$D/main/build/in"; cat "$D/main/build/screen"     # plain tools are the clients
ls "$D" "$D/main"                           # workspaces are folders, panes are folders in them
fux3 view                                   # tiles, tabs, workspaces; vim, less and htop work in a pane
# a key is a script: C-b then `v` runs an executable file you can read and replace
cat ~/.config/fux3c/keys/C-b/v              # or under $FUX3C_KEYS or $XDG_CONFIG_HOME, if you set them
echo kill > "$D/main/build/ctl"             # hangs up the program; what Unix does next is what happens
```

`./example.sh` runs the first lines for real; CI runs it on both systems.
Two notes on it: the screen is written a few milliseconds after the program
prints, so a script looks again until it sees what it waits for; and the key
scripts are written on the first `fux3 view`, to `$FUX3C_KEYS`, else
`$XDG_CONFIG_HOME/fux3c/keys`, else `~/.config/fux3c/keys`.

**Beside the merged fux3.** This one reads `FUX3C_DIR` and `FUX3C_KEYS` and
lives in `$XDG_RUNTIME_DIR/fux3c` (else `~/.cache/fux3c`) and
`~/.config/fux3c/keys`. The merged fux3 uses `FUX3_DIR` and `…/fux3`, so the
two never see each other's panes, whichever binary is called `fux3`.

## Commands

| | |
|---|---|
| `fux3 dir` | prints the session directory |
| `fux3 start [NAME] [-- PROGRAM…]` | starts a pane and prints its directory. NAME is `PANE` (in workspace `main`), `WORKSPACE/PANE`, or `WORKSPACE/` for the first free number. No program: `$SHELL`. A pane whose holder is gone is started again in place |
| `fux3 wait PANE-DIRECTORY` | waits until the pane's holder is gone, then prints `status` |
| `fux3 view [WORKSPACE]` | the viewer |
| `fux3 layout [EDIT…]` | prints the layout of workspace `$FUX3C_WS` (default `main`), or edits it (below) |

Names are ASCII letters, digits, `.`, `_` and `-`, at most 64, not starting
with a dot; a pane cannot be called `layout` or `layout.new`, in any case of
letters.

## A pane's files

| File | What it is |
|---|---|
| `in` | FIFO. Bytes written here are typed to the program |
| `ctl` | FIFO, one command a line: `kill` (hang up the program), `resize COLS ROWS [CELL-WIDTH CELL-HEIGHT]` |
| `screen` | line 1 says what rows cannot; then one line of plain text per row |
| `screen.styled` | the same with colours: text and SGR codes only, so `cat` shows it and nothing a program printed can reach your terminal as a control code |
| `status` | `pid N` (the program), `holder N`, `state running` (or `hungup`, `exited CODE`, `killed SIGNAL`), `command …` (its first 4,096 characters). Written after the screen that shows the same state |
| `history` | plain text of the lines that scrolled off the top. Past 4 MiB it becomes `history.1` and a new one starts |
| `pic-N.sixel` | a picture the program drew, whole: `cat` it in a sixel terminal |
| `log` | what this holder refused or lost and why (a command it does not know, a picture over 4 MiB, history it could not write); stops at 64 KiB |

Line 1 of both screens:

```
pid=4242 state=running v=17 size=80x24 cursor=3,8 shape=0 modes=cursor,paste pic=pic-1.sixel@0,0+12x3
```

`v` grows by one with every change written (and would start again at 0); `cursor` is row,column from 0;
`state` is `status`'s, with `:` for its space (`exited:0`);
`modes` lists what is on (`cursor` shown, `appcursor`, `keypad`, `paste`,
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

The program runs with `TERM=xterm-256color` and `FUX3C_PANE` (its pane's
directory), in the directory `fux3 start` was run in, with the rest of that
environment, the signals 1 to 31 at their defaults. Its first size is `COLUMNS`x`LINES`
if the starter set them (each 1 to 500; otherwise `log` says so), else 80x24.

Is a pane alive? Its holder keeps a lock on the directory for as long as it
lives: `flock -n "$D/main/build" true` fails while it does (Linux), and
`fux3 wait` returns when it is gone.

## The viewer

`fux3 view [WORKSPACE]` shows every folder in the workspace whose name is a
pane's name (above) as a pane. The
`layout` file in the workspace says where; a person can read and edit it, and
viewers of the same workspace share it, focus included:

```
* row(sh col(logs top):2) focus=logs
  vim focus=vim zoom
```

One tab a line. `row(…)` puts its parts side by side, `col(…)` stacks them,
`:N` is a part's share (1 to 10000; rows and columns nest at most 32 deep), `*` marks the tab shown, `focus=` is the pane that
gets the keys, `zoom` shows it alone. A folder the file does not place gets a
tab of its own; a name with no folder is dropped. A part there is no room
for (more parts than cells) is not drawn, and the viewer says so if it has
the keys. A workspace shows at most 1,000 panes. `fux3 layout EDIT` makes
one edit:

`split row|col PANE` (put PANE beside or below the focused pane) ·
`focus left|right|up|down|next|prev|PANE` · `pane PANE` (the same, for a
pane named like a direction) · `tab next|prev|N` · `break`
(the focused pane to its own tab) · `swap next|prev` · `size row|col +N|-N` ·
`zoom` · `rotate` · `arrange row|col|grid|main`

The viewer asks a pane once to take the size of its tile (through `ctl`). Two
viewers with different sizes do not fight: the last to ask wins, and the
other shows what fits.

### Keys are scripts

The keys directory is `$FUX3C_KEYS`, else `~/.config/fux3c/keys`; the viewer
writes the default scripts there if it does not exist. A key that has a file
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

A script gets `FUX3C_DIR` (the session), `FUX3C_WS` (the workspace shown),
`FUX3C_PANE` (the focused pane's directory), `FUX3C_KEYS`, `FUX3C_VIEW`,
`COLUMNS` and `LINES` (the view's size, which a pane alone in a tab has), and this
`fux3` first in `PATH`. Keys typed after it wait until it ends (two seconds
at most), so `C-b t n` and then text types into the new shell; a script that
takes longer puts its work in the background, as the defaults do.
What it prints goes to the viewer, a line at a time: `quit` leaves the
viewer, `workspace NAME` shows another workspace, and anything else (an error
from `sh`, say) is shown in the bottom right corner until the next key. A key with no file, pressed in an
open menu, closes the menu and goes nowhere. Writing the same lines to the FIFO
`$FUX3C_VIEW` does the same from anywhere.

The defaults are fux's keys, all under `keys/C-b/`: `v` split side by side,
`s` split stacked (both start in the directory the viewer was started in),
`x` then `y` close the pane (remove its directory), `z` zoom, `h j k l` focus,
`o` next pane, `n` `b` next and previous tab, `d` detach, `c` the pane's text
in the editor, `C-b` send C-b itself; `t` tabs (`n` new, `h` `l`), `w`
workspaces (`n` new, `h` `l`), `r` resize (`h j k l`, repeating), `m` move
the pane (`h j k l`, repeating), `a` pane actions (`n` to a new tab, `k` send
TERM to the program), `g` arrange (`r c g m`, `t` turn). Whether a pane
closes when its shell exits is the script's choice too: the defaults run
`fux3 wait` in the background and remove the directory.

In a menu's directory, a file `.about` holds the words shown for it in the
menu above, and a file `.repeat` keeps the menu open after each key (until a
key it has not): that is all "resize mode" is. A directory with one script,
like `x/y`, is a question.

There is no copy mode: `C-b c` puts the pane's older history, its history and
its screen in one private file and opens it at its end in `$EDITOR` (as
`$EDITOR +LINE FILE`), else `$PAGER`, else `less +G`, in a new tab; the file
is removed when the editor ends.

**The look** is fux's. A pane has every cell but the one-cell line between it
and its neighbour (`│`, `─`, a tee or a cross where lines meet); the lines
touching the focused pane are green. There are no titles and no bar: while a
prefix is in use, the last row shows the workspace, its tabs and the keys
typed so far, and the corner above it the keys that can follow. A message
(a script's words, what is wrong with `layout`) sits in the bottom right
corner until the next key. A pane whose program ended, or whose holder is
gone, says so in the corner of its own last row (`exited 0`, `lost`).

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

fux3 adds nothing to stop these. What happens is what Unix does:

- **Don't `rm -r` a pane you want.** The directory is the pane: its holder
  notices within 2 seconds, hangs up the program and leaves. That is also how
  to close one. `rm -r` of a workspace ends all its panes.
- **Don't `mv` a live pane across filesystems** (that is a copy and an `rm`).
  Within one, the pane simply has the new name; the program's `FUX3C_PANE`
  still says the old one, and so does a key script that waits to remove it.
- **Don't kill a holder with `kill -9`, or expect tidiness if you do.** The
  kernel hangs up its program. `status` goes on saying `running` and the
  FIFOs stay, with no reader: the viewer says `lost` in the pane's corner, and a plain
  `printf > in` then waits for ever, as on any FIFO nobody reads. `fux3 start`
  with the same name starts it again in place. (A holder that ends normally
  removes its FIFOs, so a late `echo kill > ctl` only leaves a small file.)
- **Don't count on `kill`, or `rm -r`, killing.** They hang up the terminal.
  A program that ignores SIGHUP lives on (after `kill`, with `state hungup`,
  and what is then written to `in` goes nowhere), and so does anything it
  started in a session of its own. `pid` is in `status`.
- **Don't remove a live pane's `in`**: that is how its holder knows the
  directory is gone, and it leaves.
- **Don't expect a pane to drop what its starter had open.** It inherits the
  open files of whoever ran `fux3 start`, as any child does (`3>&-` closes one).
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
- **Don't write nonsense to `layout`**: the viewer says what is wrong in the corner
  of its last row and shows every pane as a tab until it is fixed.

## Linux and macOS

Differences are left as they are:

- The default session directory is usually in memory on Linux
  (`$XDG_RUNTIME_DIR/fux3c`) and on disk on macOS, which sets no such
  variable (`~/.cache/fux3c`): a flood there is up to 80 small file
  replacements a second on your disk. Point `FUX3C_DIR` at a RAM disk if that
  matters.
- One write of up to 4096 bytes to a FIFO arrives whole on Linux; 512 on macOS.
- macOS ships no `flock(1)`; `fux3 wait` is the same test.
- macOS file names ignore case by default, hence `S-h` for H.
- macOS is covered by CI only (the tests, the viewer among them, and the
  example); nobody has used it there by hand.

## Coming from tmux

`ls "$(fux3 dir)"` lists the workspaces (tmux's sessions); `fux3 view NAME`
attaches to one and `C-b d` detaches; `C-b t n` starts the first shell in an
empty one. Tabs are tmux's windows. There is no chooser and no "last tab"
key: both would be scripts.

## Left out

The kitty keyboard protocol; of the mouse, the UTF-8 and urxvt report forms
(1005, 1015: programs get the SGR form or the oldest one), dragging a line
to resize, and any menu use;
hyperlinks, window titles and the clipboard (OSC 8, 0, 52 are read and
dropped); colour queries and palette changes (OSC 4, 10, 11) and the
queries DECRQSS, XTGETTCAP and XTVERSION; the bell, the visual bell and the
printer codes; reflow on resize (rows keep their text, what does not fit is
cut or goes to history); left and right margins; double underline and
underline colours (drawn as one plain underline); more than two combining
marks on a character; headings in the menu, choosers and prompts (renaming
and picking from a list would be scripts); a copy mode (`C-b c` is the
editor on the pane's text); Windows. `clear` clears the screen, not the `history` file.

Pictures: one that does not fit its tile whole is not drawn. After a picture
the cursor goes to the start of the line below it (xterm leaves it on the
picture's last row), so that a prompt does not write over it; a picture
therefore needs a free row under it, and one as tall as the pane scrolls off.

`TERM=xterm-256color` is the nearest honest name: keys reach the program
exactly as your terminal sends them, and what that terminal description
offers a program is implemented except for the items above.
