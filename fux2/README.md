# fux2

A terminal multiplexer with no server. Each pane is one small process that
owns a terminal, keeps its screen and answers whoever connects. A directory of
those panes is the whole session. The viewer is an ordinary client: start it,
kill it, start two, and the panes do not notice. It runs on Linux and macOS.

## Install and try

```sh
cargo install --path fux2 --locked
cargo install --path fux2-view --locked   # what `fux2 view` runs; keep the two side by side

fux2 start --name build -- sh
t=$(fux2 send build 'echo hi\n')
fux2 read build --since "$t" --wait 5000 --until-line hi --text
fux2 view        # C-b shows every key; C-b d leaves, the panes keep running
fux2 kill build
```

From a checkout, without installing: `cargo build -p fux2 -p fux2-view` and put
`target/debug` on your `PATH`.

## Commands

A *pane* argument is `NAME`, or `NAME@ID` for that one life of the name: if the
name has since passed to a new pane, the command is refused. Names match
`[a-z0-9][a-z0-9_-]{0,31}`.

A *token* is `<pane id>@<version>`, a place in a pane's life. `start` and `send`
print one. Mutating commands take `--if TOKEN` and are refused with `stale` if
the pane has moved on.

| Command | Example | Prints |
|---|---|---|
| `start` | `fux2 start --name build --label project=x --cols 100 --rows 30 --history 2000000 -- make` | `build <token>` |
| `ls` | `fux2 ls --label project=x --long` | a name per line; `--long` adds id, the pane daemon's pid, size, state, labels |
| `send` | `fux2 send build 'make test\n'` | `<token>` |
| `read` | `fux2 read build` | the screen as text |
| `wait` | `fux2 wait build --after "$t" --ms 5000` | `<token> <state>` |
| `resize` | `fux2 resize build 100 30` | `100 30` |
| `signal` | `fux2 signal build INT` | `foreground-group` |
| `kill` | `fux2 kill build`, or a group: `fux2 kill --label project=x` | nothing, when everything it started is gone (see below) |
| `page` | `fux2 page build` | shows the pane's scrollback in a pager (`--close-pane`: then removes the pane it runs in; the viewer's `C-b c` uses this) |
| `layout` | `fux2 layout split build tests --rows` | `version N`, then `changed NAME@ID` for each pane whose tile changed (see [The layout](#the-layout)) |
| `view` | `fux2 view` | the viewer |

With no program after `--`, `start` runs `$SHELL` (else `sh`). The
program gets the caller's directory and environment, plus `TERM=xterm-256color`,
`TERM_PROGRAM=fux2` and `FUX2_PANE=<name>`. Panes are 80x24 unless told; a size
outside 1–256 columns and 1–128 rows is a usage error (exit 2). `--history BYTES`
is the memory history may use, 1 MiB unless told, at most 16 MiB (over it: exit
6). It is memory, not text: 1 MiB kept about 95 KB of short lines in a test.
A pane that has ended still counts toward the session's 128 until `fux2 kill`.
The session directory's path must leave room for a socket name: about 100 bytes
in all on Linux and macOS, said with the numbers if it is too long.

**`send`** takes any of these, in order, as one send that is queued whole or
refused whole (`busy`, at most 64 KiB waiting):

- `TEXT`, where `\n` is the Enter key as the program's terminal mode sends it,
  `\r` a carriage return, `\\` a backslash and `\xHH` a byte
- `--key NAME`: a key: `Enter`, `Tab`, `Escape`, `Space`, `BSpace`, `Up`,
  `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`,
  `Delete`, `BTab`, `F1` to `F35`, or a character; with `C-`, `M-` or `S-` in
  front for Ctrl, Alt and Shift (`C-c`, `M-x`)
- `--paste TEXT`: a paste, bracketed if the program asked for that; its text is
  taken as it is (`\n` there is a backslash and an `n`)
- `--stdin`: standard input, byte for byte

**`signal`** names are `HUP INT TERM KILL STOP TSTP CONT WINCH QUIT USR1 USR2`
and the other POSIX names (`ABRT`, `ALRM`, ...), sent to the program's
foreground process group.

**`kill`** hangs the program up, then kills its session and
waits for all of it. On Linux it also kills anything the program started that
left its session (`setsid`), since every such process stays the pane's. On macOS
a process that left the session can't be found, and is not killed. `kill` exits
1 and names what it could not end; exit 0 means nothing it found is left.
`kill --label K=V` kills every pane that matches, and exits 0 when none does.
On Linux a list of processes that could not be read whole makes `kill` exit 1.
An ended program is reaped as soon as nothing it started is left.

**`read`** alone is a snapshot, so right after a `send` it may not show the
result yet. To wait in one command, give it the token `send` printed. Only rows
seen to change after that token count for `--until` and `--until-line`, so a line
an earlier command left on the screen is never taken for this one's. A pane
remembers its last 64 readings; a change it can no longer compare (after a resize,
say) counts as nothing new, so such a wait can time out rather than match old
output.
`--wait`, `--settle`, `--until`, `--until-line` and `--text` need `--since`, and
`--until` and `--until-line` need `--wait`:

| Flag | Meaning |
|---|---|
| `--since TOKEN` | answer with what changed since then, as JSON |
| `--wait MS` | wait for a change first; exit 5 if none comes |
| `--settle MS` | after a change, wait for this much quiet (default 100) |
| `--until TEXT` | keep waiting until a screen row contains TEXT |
| `--until-line TEXT` | keep waiting until a whole row equals TEXT (the safe choice: a typed command echoes on the screen, so `--until hi` matches `echo hi` itself) |
| `--text` | print the whole screen as text instead of JSON |
| `--json` | without `--since`: the whole screen and its metadata as JSON (`token`, `cols`, `rows`, `cursor`, `alternate`, `history` with `start`, `end` and `epoch`, `state`, `limitations`, and `screen`, a list of rows each with `row`, `text`, `rendition` and `spans`) |
| `--history-text` | all retained history and then the screen, as plain text |
| `--history EPOCH FIRST COUNT` | numbered rows as JSON, for programs; `EPOCH` and the retained range come from `--json` |

**`wait`** needs `--after TOKEN`; `--ms` is 30000 unless told, which is also the
most. At most 12 waits are parked on one pane at a time; a 13th is refused
`busy` (exit 6) at once.

A program that has ended keeps its last screen, readable until `fux2 kill`.

**Exit codes:** 0 success, 2 usage, 3 missing or replaced or gone, 4 stale,
5 timed out, 6 busy or over a limit, 7 unknown outcome (the command may or may
not have taken effect; it is never retried for you), 1 anything else, which
includes `exists` (that name is taken) and `ended` (the program has ended, so
it takes no keys). Errors go
to stderr as `fux2: <code>: <what>`.

## The layout

A session's arrangement is one file, `layout`, in its directory: workspaces,
each a list of tabs, each tab a tree of splits. Every viewer of the session
shows it, and anyone can change it: a person through the viewer's keys, an
agent through `fux2 layout`, with no viewer running. The viewer's keys run the
same commands.

```text
fux2-layout 1
version 7
next 6
workspace w1 "main"
tab t2 "build"
  match "project=x"
  split columns
    600 pane build@5f0754273d010038f09d31e5088dd85c
    400 split rows
      500 pane tests@ea7d67d849760da31b0aafc91d3be502 "unit tests"
      500 pane logs@b83e29a2f457ea668273522069cf253d
tab t3 "all"
workspace w4 "docs"
tab t5 "1"
```

A tile names one life of a pane, `NAME@ID`, so a name that passes to a new
pane never inherits a stranger's tile. Shares are relative. A pane is in one
tab: its tile's, or, if it has none, the first tab in order whose `match`
labels it carries all (a tab without `match` takes any pane); if no tab takes
it, a tab `other` appears at the end of the last workspace. Such *routed*
panes are added to their tab's tree as a grid, and get a tile of their own
the first time an edit touches that tab's tree. A pane that has ended keeps
its tile, marked `NAME ended` in its corner, or `NAME missing` once its daemon
is gone, until the tile is closed (`fux2 layout remove`) or given to another
pane (`fux2 layout replace`).

Each edit takes the layout's own lock, for at most two seconds (then `busy`,
exit 6), applies to the file as it is then, and replaces it whole: a reader
sees the old file or the new one. Starting or killing a pane never waits for
it. A file that is wrong is refused, saying where (exit 2), and one that
cannot be read, is too large (64 KiB) or is a link is refused too (exit 1);
the viewer keeps showing the last good one, and nothing is written over it
until `fux2 layout reset` (which gives every tab and workspace a new id). With
no file, a session has one tab, `all`.

**Limits.** At most 32 workspaces, 64 tabs, 256 tiles, splits nested 8 deep,
and titles of 64 bytes without control characters; a `match` test holds no
control character either. Nothing is written that a reader would refuse: an
edit that would pass a limit is refused (`limit`, exit 6), and the file stays
as it was. A version at its end is refused the same way until a reset.

**Versions.** Every edit advances the version, which edits and
`fux2 layout show` print. Every edit takes `--if VERSION` and is refused as
`stale` (exit 4), changing nothing, if the layout has moved since: so an agent
that looked, thought and then edits can tell someone rearranged things in
between. Without `--if`, an edit applies to the layout as it is now.

| Command | Does |
|---|---|
| `fux2 layout show` | prints the layout as shown: the file with routed panes added |
| `fux2 layout split PANE NEW [--rows] [--before]` | puts the existing pane `NEW` beside `PANE`, splitting its tile (side by side unless `--rows`) |
| `fux2 layout remove PANE` | takes `PANE`'s tile away (a live pane is then routed). A bare `NAME` is the live pane of that name if it has a tile, else the one tile of that name |
| `fux2 layout replace OLD NEW` | gives `OLD`'s tile to `NEW`; refused while `OLD` is still there, ended or not: kill it first |
| `fux2 layout swap PANE OTHER` | exchanges two tiles |
| `fux2 layout move PANE left\|right\|up\|down` | swaps `PANE` with its neighbour that way |
| `fux2 layout move PANE earlier\|later` | swaps `PANE` with the tile before or after it |
| `fux2 layout move PANE --tab TAB\|new` | moves `PANE` to a tab, or a new one after its own (prints `made tN`) |
| `fux2 layout move PANE --workspace WS\|new` | moves `PANE` to a workspace's first tab, or a new workspace |
| `fux2 layout border PANE DIRECTION N%\|CELLS` | moves a border of `PANE` that way, by N cells (counted in `--area`) or N% of its split: the border on that side, so `PANE` grows, or if it has none there the one on the other side, so it shrinks. Each side keeps at least a cell; a border that cannot move is refused |
| `fux2 layout zoom PANE` | shows `PANE` alone in its tab, or restores the tab |
| `fux2 layout title PANE TEXT` | names the pane `TEXT` beside its name in the bar and in lists (`""` clears it) |
| `fux2 layout tab new [--after TAB \| --workspace WS] [--title T] [--match K=V]..` | a new tab (prints `made tN`), at the end of the first workspace unless told. Routing takes the first tab in order, so a filtered tab after `all` takes nothing until it is moved earlier. Default titles are numbers, not positions |
| `fux2 layout tab rename TAB TITLE` | |
| `fux2 layout tab close TAB [--end-panes]` | the tab goes; its live panes are routed again, or with `--end-panes` ended: those the layout holds there when it closes, under its lock, printed as `ended NAME@ID`. Only a pane already gone is passed over; any other failure to end one is said (exit 1) |
| `fux2 layout tab move TAB earlier\|later` | |
| `fux2 layout tab match TAB [K=V]..` | sets the labels its routed panes carry; none takes any |
| `fux2 layout workspace new [--title T]` | a new workspace with one tab (prints `made wN`) |
| `fux2 layout workspace rename WS TITLE`, `close WS [--end-panes]`, `move WS earlier\|later` | |
| `fux2 layout reset` | the default layout, over whatever file is there |

A `TAB` is its id (`t3`) or its title if no other tab has it; a `WS` the
same (`w1`). Titles may repeat, but then only the id names them. An option a
command does not take is no option there: `tab rename all --rows` names the
tab `--rows`, and `reset --tab t2` is refused. Every edit may print `changed`
lines, a close or rename too. A pane is `NAME` or `NAME@ID`. `--area COLSxROWS` says the size
of the screen an edit is meant for: cells in `border` are counted there, and
so are the tiles `changed` names. Without it, 1000x1000.

Starting a program and placing it is two steps, and the layout never starts
or kills anything:

```sh
fux2 start --name tests -- cargo test
fux2 layout split build tests --rows
```

**Sizes.** Viewing never resizes a pane: opening a viewer, moving the focus,
resizing the viewer's terminal and a `fux2 layout` edit leave every pane's
size alone; a tile smaller than its pane cuts it off (the bar's corner says
`[cut NAME]`) and a larger one leaves blank room. The viewer's split,
move, zoom and border keys end by resizing the panes whose tiles they
changed, once, to those tiles, in that viewer only (for a mouse drag, when it
is let go). That step is `fit-after` in the command library, so a
configuration can drop it. The cost of a resize: the program redraws, a
shell's history reflows, and a token taken before it is stale for
`send --if`, so an agent that holds one should read again after a layout
change. `C-b f` fits the focused pane by hand.

## The viewer

`fux2 view` shows the stored layout and types into the focused pane. It looks
and is used as fux is: no titles and no status line, a one-cell line between
panes, drawn green around the focused pane, and the bar only while you are
using it. It sees new panes and layout changes within a second, and its own
edits at once. It starts with the first tile focused, and keeps that until you
move the focus. Each viewer keeps its own focus and its own place in the
workspaces and tabs; zoom is stored, so every viewer of a tab shows it.

**The bar** is drawn over the last row while the prefix's menu, a repeating
group, a list or a prompt is open, and the panes take that row back when it
closes. It shows the workspace, its tabs with the shown one in green, and on
the right the keys typed (`C-b …`), a repeating group's keys
(`RESIZE  h j k l · Esc`), or `[hidden …] [cut …] [zoom]` and the focused
pane's name. With nothing open, only its right corner is drawn, and only for a
notice or for panes cut off or with no room. Notices (errors in red, others in
yellow) stay until the next key; one saying a pane ended goes after five
seconds.

**Keys.** Press `C-b` and the menu of the keys after it opens at the bottom
right, under their headings; a key marked `…` opens its own. A key runs its
entry (a bound key wins over moving the selection), the arrows move and
Enter runs the one selected, Escape closes, and `C-b` again sends `C-b` to the
pane. `C-b r`, `C-b m` and the `m` menus repeat: after their first key the bar
says so (`RESIZE  h j k l · Esc`), and their keys act again without `C-b`
until Escape or Enter. Any other key ends the mode: the prefix, or another
bound key, then does what it does, and an unbound key is refused with a
notice. A paste ends a menu or a mode before it goes to the pane. Keys typed
right after a key whose binding runs wait for it, at most a second, so a list
or prompt it opens gets them, however fast they came. There is no default
key for "tab N" (fux has none); `(act "go-to-tab" N)` numbers tabs from 1 and
can be bound (see below). The default keys are fux's, after `C-b`:

| Heading | Key | Caption |
|---|---|---|
| Panes | `v` / `s` | split side by side / split stacked (a new shell beside the focused pane) |
| | `x` | close pane (Enter or `y` closes, Escape, `q` or `n` keeps it) |
| | `z` | zoom or restore |
| | `c` | history in a pager |
| | `f` | fit pane to its tile (fux2's own: fux has no such size) |
| Focus | `h` `j` `k` `l` | focus left / down / up / right |
| | `o` / `q` | next pane / last pane |
| Tabs | `n` / `b` | next tab / previous tab |
| | `t` … | Tabs: `n` new tab, `h` / `l` previous / next tab, `g` choose a tab, `r` rename tab, `x` close tab (ends its panes), `m h` / `m l` move tab earlier / later |
| Workspaces | `w` … | Workspaces: `n` new, `h` / `l` previous / next, `g` choose, `r` rename, `x` close (ends its panes), `m h` / `m l` move earlier / later |
| Session | `e` | command prompt: type grift; the answer shows in the bar's corner |
| | `d` | detach |
| | `?` | list every key (fux2's own); Enter runs the one chosen |
| Resize | `r` … | `h` `j` `k` `l`: shrink or grow left / down / up / right, a cell |
| Pane actions | `a` … | `r` rename pane (its title), `x` close pane, `k` terminate the running command, `s` swap with…, `t` / `n` move to tab… / a new tab, `w` / `W` move to workspace… / a new workspace, `m h` / `m l` move pane earlier / later |
| Move | `m` … | `h` `j` `k` `l`: move left / down / up / right (swaps with the pane that way) |

**Lists and prompts** open at the bottom right and take every key while
open, pastes too, so nothing typed there reaches a pane. A list marks where
you are with `*`: `j`/`k` or the arrows, `PageUp`/`PageDown`, `Home`/`End`
move, Enter or `y` chooses, `q`, `n` or Escape cancels, and the tab and
workspace lists rename with `r` and close with `x`. A prompt edits one line,
at most 4096 characters: `Left`/`Right`, `Home`/`End` (`C-a`/`C-e`),
`BSpace`, `Delete`, `C-u`; Enter accepts, Escape cancels. A paste it cannot
take whole (a newline, or past the limit) is refused with a notice, never cut.
The command prompt runs every form typed, in turn, and shows the last one's
value; text that does not read is an error.

**Mouse.** A click focuses the tile under it. Dragging a border moves it; it
is stored, and the panes beside it fitted, when let go. The drag is made with
`--if` the version it began at, so if the layout changed meanwhile it is
refused as stale and nothing moves; a layout change seen during the drag
ends it with a notice. Only SGR mouse reports are read (every current
terminal sends them when asked). The mouse is not forwarded to programs:
while the viewer runs, most terminals select text with Shift held.

**Whose panes.** A pane the viewer starts is started by the viewer's own
process, so it is a descendant of whatever runs the viewer. Killing a fux2
pane that runs a viewer (`fux2 kill`) also ends the panes that viewer
started, as `kill` ends everything a program left behind. Run the viewer in
an ordinary terminal to keep them apart.

A fresh viewer often shows `[cut NAME]`: panes start at 80x24, and a tile
narrower or shorter than that cuts them until a layout key or `C-b f` fits
them.

**Where fux2 looks different from fux,** because fux has nothing to copy:

- a tile larger than its pane leaves blank room, and one smaller cuts it off
  (`[cut NAME]` in the bar's corner, as fux names cut panes): fux2 never
  resizes a pane to fit by itself;
- a pane whose program ended keeps its last screen with `NAME ended` in the
  tile's corner, and a tile whose pane is gone shows `NAME missing`;
- a layout edit refused (`stale`, `busy`) or a layout file that cannot be
  read shows its message as an error notice;
- the tab and workspace lists name a tab or workspace by its id in the
  layout (`t2 all — build, logs`, `* w1 main — 1 tab, 2 panes`);
- `C-b f` and `C-b ?` are fux2's own keys.

## Scrollback and copying

The viewer has no scroll mode. A pane's history is text you read with the tools
you already have:

```sh
fux2 read build --history-text | less
fux2 read build --history-text | wl-copy     # or pbcopy on macOS
```

That prints everything the pane still retains, then its screen, each line once
and in order, however the program scrolls meanwhile. A line the terminal
wrapped comes out as the one line the program wrote. A resize or a terminal
reset during the read starts it again; after three tries it says so (`stale`).
Above 256 MiB of text it refuses (`limit`).

In the viewer, `C-b c` opens the focused pane's history in a new pane below
it, running `$PAGER`, else `less`, else `more`. Search and scroll there, and
select with your terminal to copy. If the pager can't run or fails, the pane
shows why and waits for Enter. When the pager ends, the pane removes itself
and its tile, and the focus returns.

### Keys and commands: `view.grift`

The viewer loads `fux2/view.grift` from your config directory
(`$XDG_CONFIG_HOME` or `~/.config`), at most 64 KiB, in the
[grift](https://crates.io/crates/grift) language. A binding has a caption and
a heading, and that is all the menu is made of:

```lisp
(unbind-key "C-b o")
(bind-key "F6" "next pane" "Mine" next-pane)
(bind-key "C-b g" "go to a pane" "Mine" choose-pane)
(bind-verb "C-b 1" "tab 1" "Mine" "go-to-tab" 1)   ; fux has no keys for this

(define! run-tests
  (lambda! () (send (focused) "cargo test\r")))
(bind-key "C-b y" "run the tests" "Mine" run-tests)

;; A menu of your own, under C-b i (fux leaves i free).
(bind-group "C-b i" "editors" "Mine")
(bind-key "C-b i v" "vim beside" "Mine"
  (lambda! ()
    (let! ((target (focused)) (pane (start-new "vim" "vim")))
      (place-new! pane (lambda! (p) (edit! "split" target p))))))
```

The commands are grift too: the viewer ships a library of them
(`fux2-view/src/library.grift`) and the default keys
(`fux2-view/src/defaults.grift`), loaded before your file, and your file may
redefine any of them: the default keys look a command up when they run, so
your definition is the one used. Redefine `fit-after` as
`(lambda! (text) text)`, for instance, and no layout edit resizes a pane
(`C-b f` still does). The file is read when the viewer starts.

A key is a command or a group, never both: binding a command on a key that
holds other keys, or under a key bound to a command, is refused, so every
entry of a menu can be reached by its keys. A `chord` action that leads back
to itself is stopped and said. These are the primitives they stand on:

| Word | Does |
|---|---|
| `(bind-key CHORD CAPTION HEADING f)`, `(unbind-key CHORD)` | a chord is one to four keys, `"F7"` or `"C-b t n"`; at most 256 bindings and groups |
| `(bind-verb CHORD CAPTION HEADING WORD [ARG])` | binds a chord to a viewer action by name: it acts at once, even while a binding runs |
| `(bind-group CHORD CAPTION HEADING [#true])` | names the menu a chord opens; `#true` makes its keys repeat |
| `(act WORD [ARG])` | a viewer action: `next-pane`, `previous-pane`, `last-pane`, `focus-left` (`-right`, `-up`, `-down`), `next-tab`, `previous-tab`, `last-tab`, `go-to-tab N`, `show-tab ID`, `next-workspace`, `previous-workspace`, `last-workspace`, `show-workspace ID`, `fit`, `detach`, `literal`, `chord CHORD` |
| `(focused)`, `(shown-tab)`, `(shown-workspace)`, `(area)` | the pane focused when the key was pressed, as `NAME@ID`; the tab and workspace shown (`t3`, `w1`); the tiles' room, `COLSxROWS` |
| `(panes)`, `(tabs)`, `(workspaces)`, `(key-bindings)` | lists: `(NAME@ID TAB TITLE)`, `(ID TITLE WORKSPACE)`, `(ID TITLE)`, `(CHORD CAPTION HEADING)` |
| `(arrange WORD ...)` | runs `fux2 layout WORD ... --area (area)` and gives what it printed |
| `(fit-after TEXT)` | resizes the panes `TEXT` (what `arrange` printed) names as changed, once this viewer has that version; gives `TEXT` |
| `(made TEXT)` | the tab or workspace an edit made |
| `(pick TITLE ROWS f [KEYS])` | a list of rows `(VALUE LABEL)`, with `#true` after where you are (marked `*` and selected) or `"selected"`; `f` is called with the value chosen, as a run of its own; `KEYS` are `(KEY CAPTION g)`, `g` called with the row selected |
| `(read-line PROMPT INITIAL f)` | a prompt; `f` is called with the line |
| `(say TEXT)` | a notice in the bar's corner |
| `(focus PANE)` | focus that pane (`NAME` or `NAME@ID`) when it appears within 10 s, showing its tab, and come back when it goes |
| `(start-new PREFIX [PROGRAM ARG ...])` | a new pane named `PREFIX-N`; gives `NAME@ID` |
| `(fux2-program)` | the path of the `fux2` command |
| `(send PANE TEXT)` | types TEXT literally: write `\r` for Enter |
| `(send-key PANE KEY)`, `(signal PANE NAME)`, `(resize PANE COLS ROWS)`, `(kill PANE)` | as the commands |
| `(read PANE)`, `(ls)`, `(start NAME PROGRAM ARG ...)`, `(wait PANE TOKEN MS)` | as the commands |

The library's commands, which the keys use: `edit!` (an `arrange` and then
`fit-after`), `place-new!`, `split-new`, `new-shell-here`, `history`,
`confirm`, `close-pane`, `confirm-close-pane`, `choose-pane`, `choose-tab`,
`choose-workspace`, `swap-with`, `move-to-tab`, `move-to-workspace`,
`move-to-new-tab`, `move-to-new-workspace`, `rename-pane`, `rename-tab`,
`rename-workspace`, `new-tab`, `new-workspace`, `confirm-close-tab`,
`confirm-close-workspace`, `command-line`, `key-list`, `drag-border` (what a
mouse drag does when let go), and a word for each viewer action.

`place-new!` places a pane just started; if the edit loses a race it tries
once more, and if that fails too it raises `not-placed`, saying the pane is
running and has no tile. It never starts a second one or kills the first.

A word that fails raises an error that names what went wrong, shown as an
error notice. A binding copies at most 16 MiB of text out of the language in
all, and a command's output is at most 8 MiB. Loading gets 2 seconds for each
of the library, the default keys and your file, and each binding or answer
from a list or prompt gets 5, on a thread of its own: the viewer keeps drawing
and taking keys, and `C-b d` works, while one runs. One that overruns is
stopped and a notice says so. A list or prompt waits for you without
using a binding's time.

## Where panes live, and who may connect

The session directory is `$XDG_RUNTIME_DIR/fux2/default` on Linux (else
`~/.cache/fux2/default`) and `~/Library/Caches/fux2/default` on macOS, or the
absolute private directory
`FUX2_DIR` names. A session holds at most 128 panes. It is refused unless it is a
real directory, yours and closed to others.

**Any process running as your user can connect, and a process that connects can
read every pane's screen, type into it and kill it.** Other users are refused. Tokens and pane ids
prevent mistakes such as typing into a replaced pane. They are not passwords.

## What it leaves out

No ordering across panes, no restart of a program, no taking over of a pane
from a crashed daemon, no graphics, no clipboard bridge, no program-set
palette colours, and no judgement of whether a program is "ready". If a
pane's daemon itself is killed its screen is gone, and its program normally
gets a hang-up; a program that ignores that keeps running with nothing
attached.

Left out of the viewer, on purpose for now:

- **a copy mode**: history is text in a pager (`C-b c`), and your terminal
  selects;
- **the mouse forwarded to programs**: the viewer keeps it for focus and
  borders;
- **respawning a program in place**: a pane does not say what program it
  runs, so the library cannot start it again; `fux2 kill`, `fux2 start` and
  `fux2 layout replace OLD NEW` do it by hand;
- **dumping and replaying a session**: the layout file is the saved
  arrangement, and it outlives every viewer.

fux2 does not run on Windows; building it there fails with a message saying so.
