# fux3-view

fux3's viewer: it shows a fux3 session's panes the way fux shows its own, and
sends your keys to the focused one. It reads and writes only the pane files
(see `fux3/DESIGN.md`) and its own `.view/` directory in the session. It owns
nothing: kill it however you like and every pane goes on. Start two and they
show the same layout; each keeps its own focus and shown tab.

```sh
D=$(fux3 dir)
fux3 start build -- sh
fux3 view                                   # runs fux3-view with FUX3_DIR set
echo "split build right" > "$D/.view/ctl"   # every running viewer changes
fux3 view --do 'zoom build'                 # the same with no viewer running
```

## What it reads and writes

| Path | What the viewer does with it |
|---|---|
| `NAME/screen.styled` | drawn in the pane's tile; looked at every 20 ms while shown, and read again only when replaced (its inode, size or time changed) |
| `NAME/status` | the pane's id, labels, state, size, key mode and newline mode; looked at the same way |
| `NAME/images/N.sixel` | the pictures the screen names, drawn in sixel, cropped to the cells that show, when this terminal's DA1 says it draws sixel; else the cells stay blank |
| `NAME/in` | your keys, encoded for the mode `status` gives (`keys`); Enter is CR LF under `newline-mode on`; at most 512 bytes a write, queued (64 KiB at most) while the pipe is full |
| `NAME/ctl` | `resize COLS ROWS` (only the fit step below), `paste PATH`, and whatever your grift sends (`kill`, `signal NAME`) |
| `NAME/history` | paged in a pane of its own by `C-b c` |
| `.view/layout`, `.view/layout.lock` | the layout, shared by every viewer (below) |
| `.view/ctl` | a FIFO of layout commands, made by the first viewer; every viewer reads it, so any one takes a line |
| `.view/errors` | the last 32 refused `.view/ctl` lines, `LINE: WHY`, replaced whole |
| `.view/paste-PID-N` | a paste's text, 0600, removed once the pane's `commands` count passed the `paste` command, or after 10 s |

The session directory is listed again once a second and whenever the layout
changes. A pane whose directory is gone keeps its tile, drawn as `NAME gone`;
one whose program ended keeps it too, marked `NAME exited 0` (or `signalled
…`). A new pane under an old name gets its own tile: tiles name a pane with its
id. Panes the viewer starts get the first free `PREFIX-N` that no pane and no
tile uses.

## The layout file

`.view/layout` holds workspaces, tabs, and per tab a tree of splits with
relative shares over panes named `NAME@ID`, a zoom, and a filter:

```
fux3-layout 1
version 7
next 5
workspace w1 "main"
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
filter takes any), else in a tab `other` at the end of the first workspace.
Every change is one command applied to the file as it is then, under the lock
(waited for 2 s at most), and written whole by atomic replace; `version` goes
up by one. A missing (after one was read), oversized (over 64 KiB) or wrong
file is refused out loud: the viewer keeps showing the last good layout and
says why in the bar's corner, and no command writes over it. `reset` is the
one command that does.

## Layout commands

One line each, written to `.view/ctl` (a viewer takes it), run with
`fux3-view --do 'LINE'` (prints what it did; a refusal goes to stderr, exit 1),
or from grift with `(arrange LINE)`. `PANE` is `NAME` or `NAME@ID`; `TAB` is
`tN` or a title no other tab has; `WS` likewise `wN` or a title; `DIR` is
`left`, `right`, `up` or `down`; a title with spaces is quoted, `"like this"`.

| Command | What it does |
|---|---|
| `split PANE DIR` | starts a shell (`shell-N`, your `$SHELL`) and puts it beside PANE on side DIR |
| `split PANE DIR NEW` | puts the existing pane NEW beside PANE on side DIR |
| `close PANE` | takes PANE's tile away (a pane still running comes back where its filter routes it) |
| `swap PANE PANE` | the two exchange tiles |
| `move PANE DIR` | swaps PANE with the pane beside it that way |
| `move PANE earlier` / `later` | swaps PANE with the one before or after it in its tab |
| `move PANE tab TAB` / `tab new` | moves PANE into a tab, or a new tab after its own |
| `move PANE workspace WS` / `workspace new` | moves PANE into a workspace's first tab, or a new workspace |
| `border PANE DIR CELLS` | moves PANE's border toward DIR by CELLS cells (as seen in the screen it was given in) |
| `zoom PANE` | shows PANE alone in its tab, or restores the tab |
| `tab new [TITLE] [after TAB] [match KEY=VALUE…]` | a new tab |
| `tab rename TAB TITLE`, `tab close TAB`, `tab move TAB earlier\|later` | |
| `workspace new [TITLE]`, `workspace rename WS TITLE`, `workspace close WS`, `workspace move WS earlier\|later` | |
| `reset` | the default layout (one workspace `main`, one tab `all`), over whatever is there |

Any command may end with `if VERSION`: it is refused unless the layout is at
that version (a mouse drag uses it). What a command did is printed as lines:
`version N`, `made tN`/`made wN`, `started NAME@ID`, and `changed NAME@ID` for
every pane whose tile moved or changed size.

**Sizes.** Opening a viewer, moving the focus and resizing the viewer's
terminal resize no pane: a tile clips or pads it (`[cut NAME]` in the corner
says a pane is larger than its tile). The shipped commands end by fitting the
panes they changed, once (`resize COLS ROWS` on each one's `ctl`, the size of
its tile in the viewer that ran the command): that step is the library's
`edit!`; `.view/ctl` lines go through `layout-command`, which is `edit!`.
Define `layout-command` as `arrange` in your config to leave sizes alone. `C-b
f` fits the focused pane by hand. `--do` fits nothing.

## Keys

fux's keys, wherever fux3 has the command. `C-b` opens the prefix menu, drawn
from the bindings alone (each has a caption and a heading); a key bound to a
group opens that group's menu, and Escape closes it. `C-b` twice sends `C-b`.

| Keys | |
|---|---|
| `C-b v` / `C-b s` | split side by side / stacked (a new shell) |
| `C-b x`, `C-b z`, `C-b c`, `C-b f` | close pane (asks), zoom or restore, history in a pager, fit the pane to its tile |
| `C-b h j k l`, `C-b o`, `C-b q` | focus left/down/up/right, next pane, last pane |
| `C-b n`, `C-b b` | next / previous tab |
| `C-b t n h l g r x`, `C-b t m h l` | tabs: new, previous, next, choose, rename, close; reorder (repeats) |
| `C-b w n h l g r x`, `C-b w m h l` | workspaces: the same |
| `C-b r h j k l` | move a border (repeats) |
| `C-b m h j k l` | move the pane (repeats) |
| `C-b a x k s t n w W`, `C-b a m h l` | pane actions: close, terminate (`signal TERM`), swap with…, move to tab…, to a new tab, to workspace…, to a new workspace; reorder |
| `C-b e`, `C-b ?`, `C-b d` | grift prompt, every key, detach |

Mouse: a click focuses a pane; dragging a border moves it when let go. Mouse
events are never sent to programs. A paste goes to the focused pane through
`paste` (bracketed if the program asked).

## Configuration: grift

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
| `(bind-group KEYS CAPTION HEADING [REPEAT])`, `(unbind-key KEYS)` | a group's menu; REPEAT `#true` keeps its keys active |
| `(act VERB [ARG])` | verbs: `next-pane`, `last-pane`, `focus-left` … `focus-down`, `next-tab`, `previous-tab`, `next-workspace`, `previous-workspace`, `show-tab "tN"`, `show-workspace "wN"`, `fit`, `detach`, `chord "C-b v"` |
| `(focused)`, `(shown-tab)`, `(shown-workspace)` | `NAME@ID`, `tN`, `wN` |
| `(workspaces)`, `(tabs)`, `(panes)`, `(key-bindings)` | lists of rows: `(wN title)`, `(tN title wN)`, `(NAME@ID tN)`, `(keys caption heading)` |
| `(arrange LINE)` | one layout command; its result lines |
| `(fit-after RESULT)`, `(line-of KEY RESULT)` | fit the panes a result changed; a result line's value |
| `(start-new PREFIX PROGRAM ARG…)` | starts `PREFIX-N` (your shell if no program): `NAME@ID` |
| `(send PANE TEXT)`, `(send-key PANE KEY)` | to the pane's `in`, after keys typed before |
| `(ctl PANE LINE)` | one line to the pane's `ctl` (`kill`, `signal INT`, …) |
| `(pick TITLE ROWS FUNCTION [KEYS])`, `(read-line PROMPT INITIAL FUNCTION)` | a list or a line to type; what is chosen goes to FUNCTION |
| `(focus PANE)`, `(say TEXT)`, `(session-dir)`, `(quote TEXT)` | |

```lisp
;; ~/.config/fux3/view.grift
(bind-key "F7" "run the tests" "Mine"
  (lambda! () (send (focused) "cargo test\r")))
(unbind-key "C-b n")
(define! layout-command (lambda! (line) (arrange line)))   ; .view/ctl resizes nothing
```

## Where fux3's viewer differs from fux

- **No server.** Panes are fux3 holders; the viewer reads their files, so a
  pane appears within a second of its directory, and a screen within about
  20 ms of its publish (fux3 publishes at most 25 times a second).
- **Sizes stay.** A pane keeps its size when viewed, focused, or when the
  terminal is resized; only the split, move and border commands (and `C-b f`)
  fit it, once. fux resizes panes to their tiles.
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
