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
| `ls` | `fux2 ls --label project=x --long` | a name per line; `--long` adds id, pid, size, state, labels |
| `send` | `fux2 send build 'make test\n'` | `<token>` |
| `read` | `fux2 read build` | the screen as text |
| `wait` | `fux2 wait build --after "$t" --ms 5000` | `<token> <state>` |
| `resize` | `fux2 resize build 100 30` | `100 30` |
| `signal` | `fux2 signal build INT` | `foreground-group` |
| `kill` | `fux2 kill build`, or a group: `fux2 kill --label project=x` | nothing, when everything it started is gone (see below) |
| `page` | `fux2 page build` | shows the pane's scrollback in a pager (`--close-pane`: then removes the pane it runs in; the viewer's `C-b [` uses this) |
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
next 5
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
it, a tab `other` appears at the end of the first workspace. Such *routed*
panes are added to their tab's tree as a grid, and get a tile of their own
the first time an edit touches that tab's tree. A pane that has ended keeps
its tile, shown `[ended]`, or `[missing]` once its daemon is gone, until the
tile is closed (`fux2 layout remove`) or given to another pane
(`fux2 layout replace`).

Each edit takes the layout's own lock, for at most two seconds (then `busy`,
exit 6), applies to the file as it is then, and replaces it whole: a reader
sees the old file or the new one. Starting or killing a pane never waits for
it. A file that is too large (64 KiB) or wrong is refused, saying where; the
viewer keeps showing the last good one, and nothing is written over it until
`fux2 layout reset`. With no file, a session has one tab, `all`.

**Versions.** Every edit advances the version, which edits and
`fux2 layout show` print. Every edit takes `--if VERSION` and is refused as
`stale` (exit 4), changing nothing, if the layout has moved since: so an agent
that looked, thought and then edits can tell someone rearranged things in
between. Without `--if`, an edit applies to the layout as it is now.

| Command | Does |
|---|---|
| `fux2 layout show` | prints the layout as shown: the file with routed panes added |
| `fux2 layout split PANE NEW [--rows] [--before]` | puts the existing pane `NEW` beside `PANE`, splitting its tile (side by side unless `--rows`) |
| `fux2 layout remove PANE` | takes `PANE`'s tile away (a live pane is then routed) |
| `fux2 layout replace OLD NEW` | gives `OLD`'s tile to `NEW` |
| `fux2 layout swap PANE OTHER` | exchanges two tiles |
| `fux2 layout move PANE left\|right\|up\|down` | swaps `PANE` with its neighbour that way |
| `fux2 layout move PANE earlier\|later` | swaps `PANE` with the tile before or after it |
| `fux2 layout move PANE --tab TAB\|new` | moves `PANE` to a tab, or a new one after its own (prints `made tN`) |
| `fux2 layout move PANE --workspace WS\|new` | moves `PANE` to a workspace's first tab, or a new workspace |
| `fux2 layout border PANE DIRECTION N%\|CELLS` | moves the border on that side of `PANE` that way (the other side's if it has none there) |
| `fux2 layout zoom PANE` | shows `PANE` alone in its tab, or restores the tab |
| `fux2 layout title PANE TEXT` | shows `TEXT` in `PANE`'s title bar (`""` clears it) |
| `fux2 layout tab new [--after TAB \| --workspace WS] [--title T] [--match K=V]..` | a new tab (prints `made tN`) |
| `fux2 layout tab rename TAB TITLE` | |
| `fux2 layout tab close TAB` | the tab goes; its live panes are routed again |
| `fux2 layout tab move TAB earlier\|later` | |
| `fux2 layout tab match TAB [K=V]..` | sets the labels its routed panes carry; none takes any |
| `fux2 layout workspace new [--title T]` | a new workspace with one tab (prints `made wN`) |
| `fux2 layout workspace rename WS TITLE`, `close WS`, `move WS earlier\|later` | |
| `fux2 layout reset` | the default layout, over whatever file is there |

A `TAB` is its id (`t3`) or its title if no other tab has it; a `WS` the
same (`w1`). A pane is `NAME` or `NAME@ID`. `--area COLSxROWS` says the size
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
size alone; a tile smaller than its pane clips it (the title says
`[clipped 80x24]`) and a larger one leaves blank room. The viewer's split,
move, zoom and border keys end by resizing the panes whose tiles they
changed, once, to those tiles, in that viewer only (for a mouse drag, when it
is let go). That step is `fit-after` in the command library, so a
configuration can drop it. The cost of a resize: the program redraws, a
shell's history reflows, and a token taken before it is stale for
`send --if`, so an agent that holds one should read again after a layout
change. `C-b f` fits the focused pane by hand.

## The viewer

`fux2 view` shows the stored layout and types into the focused pane. It sees
new panes and layout changes within a second, and its own edits at once. It
starts with the first tile focused, and keeps that until you move the focus.
The focused pane's title bar is drawn in reverse video; the others are dim.
The status line says which workspace and tab are shown and how many there
are, such as `main 1/2 | build 1/3`. Each viewer keeps its own focus and its
own place in the workspaces and tabs.

**Keys.** Press `C-b` and a menu of the keys after it appears, under their
headings; a key marked `…` opens its own menu. Escape closes a menu. `C-b r`
and `C-b m` (and the reorder menus) repeat: their keys act again without
`C-b` until another key. `C-b ?` lists every key; type to narrow it, and
Enter runs the one chosen. These are the default keys, all after `C-b`:

| Heading | Key | Caption |
|---|---|---|
| Panes | `v` / `s` | split side by side / split stacked (a new shell beside the focused pane) |
| | `c` | new shell in this tab |
| | `x` | close pane (Enter or `yes` closes, Escape keeps it) |
| | `z` | zoom or restore |
| | `[` | history in a pager |
| | `f` | fit pane to its tile |
| | `a` … | pane actions |
| Pane actions | `a r` | rename pane (its title) |
| | `a x` | close pane |
| | `a k` | terminate its program (`TERM`) |
| | `a s` | swap with… |
| | `a t` / `a n` | move to tab… / move to a new tab |
| | `a w` / `a W` | move to workspace… / move to a new workspace |
| | `a m h` / `a m l` | move pane earlier / later (repeats) |
| Focus | `o` / `O` | next pane / previous pane |
| | `;` | last pane |
| | `Left` `Down` `Up` `Right` | focus left / down / up / right |
| | `g` | go to a pane… |
| Layout | `r h` `r j` `r k` `r l` | border left / down / up / right, a cell (repeats) |
| | `m h` `m j` `m k` `m l` | move left / down / up / right (repeats) |
| Tabs | `n` / `p` / `l` | next tab / previous tab / last tab |
| | `1` … `9` | tab 1 … tab 9 |
| | `t n` / `t g` | new tab / choose a tab… |
| | `t r` / `t x` | rename tab / close tab (ends its panes) |
| | `t m h` / `t m l` | move tab earlier / later (repeats) |
| Workspaces | `)` / `(` / `L` | next workspace / previous workspace / last workspace |
| | `w n` / `w g` | new workspace / choose a workspace… |
| | `w r` / `w x` | rename workspace / close workspace (ends its panes) |
| | `w h` / `w l` | previous workspace / next workspace |
| | `w m h` / `w m l` | move workspace earlier / later (repeats) |
| Session | `:` | command line: type grift, the answer shows on the status line |
| | `?` | list every key |
| | `d` | detach |
| | `C-b` | send C-b to the pane |

**Lists and prompts** take every key while open, pastes too, so nothing
typed there reaches a pane. In a list, type to narrow it, `Up`/`Down`
(`C-p`/`C-n`), `PageUp`/`PageDown`, `Home`/`End` move, Enter chooses, Escape
cancels. A prompt edits one line: `Left`/`Right`, `Home`/`End` (`C-a`/`C-e`),
`BSpace`, `Delete`, `C-u`; Enter accepts, Escape cancels.

**Mouse.** A click focuses the tile under it. Dragging a border (the line
between tiles side by side, or a tile's title bar for tiles stacked) moves it;
it is stored, and the panes beside it fitted, when let go. The drag is made
with `--if` the version it began at, so if the layout changed meanwhile it is
refused as stale and nothing moves. The mouse is not forwarded to programs:
while the viewer runs, most terminals select text with Shift held.

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

In the viewer, `C-b [` opens the focused pane's history in a new pane below
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

(define! run-tests
  (lambda! () (send (focused) "cargo test\r")))
(bind-key "C-b y" "run the tests" "Mine" run-tests)

;; A menu of your own, under C-b e.
(bind-group "C-b e" "editors" "Mine")
(bind-key "C-b e v" "vim beside" "Mine"
  (lambda! ()
    (let! ((target (focused)) (pane (start-new "vim" "vim")))
      (place-new! pane (lambda! (p) (edit! "split" target p))))))
```

The commands are grift too: the viewer ships a library of them
(`fux2-view/src/library.grift`) and the default keys
(`fux2-view/src/defaults.grift`), loaded before your file, and your file may
redefine any of them. Redefine `fit-after` as `(lambda! (text) text)`, for
instance, and no key resizes a pane. These are the primitives they stand on:

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
| `(pick TITLE ITEMS f)` | a list of `(VALUE LABEL)`; `f` is called with the value chosen, as a run of its own |
| `(read-line PROMPT INITIAL f)` | a prompt; `f` is called with the line |
| `(say TEXT)` | a message on the status line |
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

A word that fails raises an error that names what went wrong, shown on the
status line. A binding copies at most 16 MiB of text out of the language in
all, and a command's output is at most 8 MiB. Loading gets 2 seconds for each
of the library, the default keys and your file, and each binding or answer
from a list or prompt gets 5, on a thread of its own: the viewer keeps drawing
and taking keys, and `C-b d` works, while one runs. One that overruns is
stopped and the status line says so. A list or prompt waits for you without
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

- **a copy mode**: history is text in a pager (`C-b [`), and your terminal
  selects;
- **the mouse forwarded to programs**: the viewer keeps it for focus and
  borders;
- **respawning a program in place**: a pane does not say what program it
  runs, so the library cannot start it again; `fux2 kill`, `fux2 start` and
  `fux2 layout replace OLD NEW` do it by hand;
- **dumping and replaying a session**: the layout file is the saved
  arrangement, and it outlives every viewer.

fux2 does not run on Windows; building it there fails with a message saying so.
