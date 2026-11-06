# fux

A minimal terminal multiplexer, driven and configured entirely in
**grift**, a small Lisp built on fexprs. One server keeps your programs running in
panes, laid out in tabs and workspaces; `fux` attaches a terminal to it,
and several terminals can attach at once, each with its own view and its
own keys. Every other `fux` command line is a short grift program, and so
are fux's config, key bindings, command prompt and saved sessions.

fux is made for agents first, and the same operations serve people. A program
driving fux reads every fact as a record; learns every multiplexer and
program-lifecycle change from a journal of events, and every screen
change from versions and captures; gets live process facts from the OS;
and acts only on the screen it saw. fux reports facts (a version moved,
a program exited with 7, a text is on the screen) and never judges
whether a program is ready, idle or done. What the OS already does, the
OS does: `env`, `timeout`, `kill`, `ps`, your terminal's clipboard and
your editor.

fux is one binary for macOS, Linux and Windows 11, built with Rust 1.95 or later:
`cargo install --locked fux`, or in a clone `cargo install --locked --path
.`. `fux` alone attaches, starting a server if none runs.

On Windows a pane's shell is `%COMSPEC%`, else `cmd.exe`, and the config is
`%APPDATA%\fux\config.grift`; for PowerShell, `(set "shell" (list "pwsh"))` there, and
`[Environment]::CurrentDirectory = $PWD.ProviderPath` in its prompt, so a split starts
where you are. fux fetches Microsoft's ConPTY on first use and says so;
[WINDOWS.md](WINDOWS.md) says what differs.

- `fux '(explain "agents")'` is the page for driving programs from a script or an
  agent ([docs/agents.md](docs/agents.md)), `fux '(explain "TOPIC")'` the rest.
- `fux '(explain "cookbook")'` has short recipes to copy, each run by fux's tests
  ([docs/cookbook.md](docs/cookbook.md)).
- `fux '(help)'` lists every operation, `fux '(explain "split")'` says one at length,
  `fux '(explain "data")'` says how data prints, and [docs/records.md](docs/records.md)
  gives every record, field, event and failure.

## Using it

`fux` attaches to the server, starting one in the background if none
answers, and `C-b d` detaches: the programs keep running. `fux '(attach
(in (options) "work"))'` attaches to workspace `work`, `fux '(attach (tab 2))'`
shows a tab, and `fux '(attach (pane 3))'` focuses a pane; each must exist.
`fux '(attach (watch (options)))'` only watches: its keys do nothing but `C-b d`,
and it never starts a server. Inside a fux pane, `fux` refuses to attach.

- **Panes** are terminals that outlive their programs. A pane runs your
  shell (the `shell` setting: your `$SHELL`, else `/bin/sh`), or the
  program it was given. A shell's pane closes when its shell exits; a
  program's stays, its exit told, until `kill` closes it or `respawn`
  starts its program again. A pane's lines re-wrap when its width
  changes, history included.
- **Tabs** lay out panes, split side by side or stacked one at a time. A
  tab closes with its last pane.
- **Workspaces** hold tabs and workspaces, so workspaces nest: a project
  `proj` with a workspace per agent shows as `proj › agent1`. A workspace
  closes with its last child.
- **Each client is shown its own** workspace, tab and pane, and remembers
  the one it was shown before at each level. A tab's layout may hold
  **stacks**: `(stack PANE)` puts a new pane on that pane's rectangle, as
  `split` puts one beside it, and each client is shown one child of a
  stack, its own. Every child keeps its size, so choosing one resizes no
  program. In a stack, `C-b o` shows the next child, `C-b q` the last.
- **The bar** is hidden: panes have every row. While the prefix, or any
  screen of fux's, is in use, the bar draws over the bottom row, with the
  workspace's path and its tabs on the left and the focused pane or what
  the keys do on the right; when it ends, the row shows the pane again. A
  bell or a program's notification (OSC 9, 777) shows in the bottom-right
  corner for five seconds, or until your next key. When a workspace you
  are in closes by someone else's request (a kill, or its last program
  ending), the corner says what closed, who closed it and where you are
  now, until your next key: `harmony › meta-design killed by %12: 20 tabs
  closed · now harmony › desktop-simple · C-b w g shows all`. A screen
  too small for its tab shows the panes cut off, and the corner names
  them (`[hidden %3] [cut %2]`). No pane is ever resized for fux's own
  feedback. To give the bar a row of its own, see the cookbook's "The bar
  on a row of its own".

The server runs until its last pane closes, until `fux '(kill-server)'`
(refused from its own panes and while a person's terminal is attached), or
until SIGTERM, SIGINT or SIGHUP, which `kill` sends ([Security](#security)
shows how on Linux and macOS). It then hangs up every pane, tells each
attached terminal why, and removes its socket. Each pane runs in a process
of its own (`fux __pane`), holding its terminal, and each client's
connection in another (`fux __conn`), which composes its client's screen
from the panes' screens it gets from them, and keeps what its client sends
until the panes take it: a pane that floods its memory, or a client that
stops reading, ends only its own process. The server holds the layout,
names and the journal, and no screen or paste. A server that
crashes, or is killed with `kill -9` or by the OOM killer, ends alone: its
panes' processes wait, with their programs, and `fux '(start)'` on its socket
takes the run back, as it was.
Nothing is saved on its own ([Saved sessions](#saved-sessions)).

## Keys

Every key below follows the prefix, `C-b`. Case counts: `C-b T` is not
`C-b t`. The keys are your client's: each terminal attached has its own
keymap, from its config, and the server knows none of it. A key that is
not fux's reaches the focused pane as you typed it.

Keymaps nest, as Emacs's prefix maps do: a key maps to a binding or to
another keymap, at any depth. The prefix is a key of the root map that
holds the map of the keys below, and `C-b t` holds the tab keys in that
one. A key that holds a map opens the **command column**, headed by the
keys typed so far and listing that map's keys: the arrows move, Enter
runs the selected one, a key runs its own, Esc closes. The prefix again,
where its map doesn't bind it, sends it to the pane.

| Key | Runs | Does |
| --- | --- | --- |
| `h` `j` `k` `l` | `(step (me) (focused (me)) "left")` … | focus the pane that way |
| `o` / `q` | `(step (me) (focused (me)) "next")` / `… "last"` | focus the next / last pane (in a stack, child) |
| `v` / `s` | `(split (axis (focused (me)) "horizontal"))` / `… "vertical"` | split side by side / stacked |
| `x` | `(confirm-close (me) (focused (me)))` | close the pane (`y` or Enter closes, `n` or Esc keeps it) |
| `z` | `(zoom (focused (me)))` | zoom the pane, or restore it, on every screen showing its tab |
| `c` | `(edit-history (me))` | the pane's history in your `$EDITOR` |
| `a`, then a key | `(rename-prompt (me) (focused (me)))` … | pane actions: `r` rename, `x` close, `k` terminate the running command, `s` swap, `t`/`w` move to a tab/workspace, `n`/`W` move to a new one, `m h`/`m l` reorder |
| `n` / `b` | `(step (me) (tab (focused (me))) "next")` / `… "previous"` | next / previous tab |
| `t`, `w` | the tab and workspace maps | `n` new, `h`/`l` previous and next, `g` choose, `r` rename, `x` close, `m h`/`m l` move; `w j`/`w k` into a workspace in this one and out of it |
| `e` | `(command-prompt (me))` | type any grift |
| `d` | `(detach (me))` | detach this terminal |
| `r`, then `h` `j` `k` `l` | `(move-border (focused (me)) "left" 1)` … | move a border one cell |
| `m`, then `h` `j` `k` `l` | `(move (focused (me)) "left")` … | move the pane beside its neighbour |

`r` and `m` are maps that **repeat**: `h` `j` `k` `l` act again without
the prefix until Esc or Enter. `f`, `g`, `i`, `u` and `y` are free for
your own. `(me)` is the client whose key or prompt runs the code; `"last"`
toggles with the one this client was shown before. `confirm-close` and
`rename-prompt` are operations of fux's library, as `step` is of its core:
your config and the command prompt have them, and a command line has them
too, though it has no `(me)` to give them.

- **`C-b c`** opens the focused pane's history in your editor (the
  attached terminal's `$EDITOR`, else `vi`) at its last line, in a split
  titled `history` that closes when the editor exits: search, select and
  copy there. It stands in for a copy mode and a pager.
- **Choosers** (`t g`, `w g`, `a s`, `a t`, `a w`) list tabs, workspaces
  or panes. `w g` lists every workspace on the server, each indented
  under what holds it, with the tabs, panes and workspaces it holds
  counted and `*` on yours and each holding it; the others list the
  siblings of yours. Enter or `y` selects, `r` renames, `x` closes, Esc,
  `q` or `n` cancels. They are library programs over one `choose`
  primitive (`choose-among`, `choose-move`, `choose-pane`; `fux '(explain
  "choose")'`).
- **The command prompt** (`e`) runs grift as your client, `(me)` and all,
  and shows what it printed, or its failure, in the bar. `prompt` opens
  a line of your own whose Enter runs your code with the line.
- **A key's program** runs as your client, one at a time; while one runs
  that may open a screen, the keys you type wait for it. The prefix, then
  Esc, stops one that takes long.

The keys are a config, run before yours, in the operations yours uses
([src/script/keys.grift](src/script/keys.grift)): `fux '(list-keys)'` prints them
as config lines ([Configuration](#configuration)).

## Driving it

A command line is one quoted grift program: its forms run in order, and
the last one's value is printed. `(pane 3)`, `(tab 2)` and `(workspace 1)`
are a pane, a tab and a workspace, and `(pane "NAME")` finds a pane by its
name. Every shell argument after the program stays a string, read with
`(arguments)`, so text from the shell is never source. A final list prints
an item a line; a capture prints as its text, and a record as one line of
grift data.

```sh
fux '(rename (new-tab (program (size (in (options) "agent") 80 24) "sh")) "sh1")'
A=$(fux '(line (pane "sh1") (first (arguments)))' 'sleep 1; echo "six times seven: $((6*7))"')
fux '(wait (changed (first (arguments))))' "$A"
fux '(shows (pane "sh1") "seven: 42" 30)'
fux '(define! seen (capture (pane "sh1"))) (list (get seen "version") (get seen "cursor") (get seen "program"))'
fux '(listing (list-session))'
```

The first line starts a shell in a new tab in workspace `agent`, made if
it isn't there, and names its pane `sh1`; from outside fux a new tab
needs a size, as fux makes none up. What an action takes (the program,
keys, a size) wraps what it acts on: `(options)` for a new tab, or the
pane, and each option gives back what it was given, with itself added.

Every pane has a **version**, one string that names the pane, counts what
only grows while its program runs (`respawns`, `screen`, its revision,
`bells`, `done`, shell commands ended, and `ended`, each a field by that
name too) and names the server's run. An
action (`new-tab`, `line`, `send-keys`, `signal` …) acts at once and
answers its pane's version from just before it acted; `(wait (changed V))`
waits until the version differs, and says what changed. `shows`, `quiet`
and `until-ended` wait for a text, for a quiet screen and for the
program's end, and `expect` sends keys only if the pane is still at the
version you read. fux never judges whether a program is done: `fux
'(explain "agents")'` says what to wait for, and `fux '(explain "recipes")'` how.

A failure prints nothing more on stdout: stderr says `fux: KIND: VALUES`,
with the form, file and line it came from, then what had already taken
effect. fux exits 1
for a refusal; 2 for a mistake in the program (an unbound name, a value of
the wrong type, a form that doesn't read …); 4 if the pane waited on closed; 124 if nothing changed
in time; and 141, quietly, if stdout's reader went (`| head -n 1`). The
first argument is a whole program, never a file; `fux
--file FILE ARGS…` runs a file, so a script can start with `#!/usr/bin/env
-S fux --file`, and `fux -` runs what stdin says.

A handle names everything under it: `information`, `dump`, `events`, `panes`,
`tabs` and `kill` take a pane, a tab, a workspace or `(all)`, and apply to
the whole of it, so `kill` on a project closes its agents' workspaces too.

```sh
fux '(new-tab (program (size (in (in (options) "proj") "agent1") 80 24) "sh"))' > /dev/null
fux '(new-tab (program (size (in (in (options) "proj") "agent2") 80 24) "sh"))' > /dev/null
fux '(length (panes (workspace "proj")))'
fux '(kill (workspace "proj"))'
```

Names are given, never made: `name` and `rename` give one, and a
workspace, tab or pane that has a name refuses it to another
(`name-taken`), so a name finds one thing. fux sets no limits of its own
on memory, time or steps: `ulimit`, cgroups and `timeout` do.

## grift

grift is a Lisp with one primitive operative, `vau!`, and no macros: `if!`
and `lambda!` are definitions you can read. Everything is written out in
full English words: `(define! NAME VALUE)` names a value, `(lambda! (x)
...)` makes a function, `if!`, `when!` and `conditional!` decide, recursion
repeats, and `(catch-kind! EXPRESSION timeout failure ...)` handles a
failure. The whole program is read first, so a form that doesn't read runs
nothing; after that, forms run in order and each operation checks its values
as it runs, so a failure comes after the forms before it have acted, and
stderr lists what took effect. `fux '(help)'` lists fux's operations and
its library's, which are written in grift themselves (`fux '(explain
"shows")'` shows each one's code); [grift's own
README](https://github.com/gold-silver-copper/grift) and its `docs/LANGUAGE.md`
are the language at length.

```sh
fux '(define! double (lambda! (x) (multiply x 2))) (double 21)'
fux '(get (word "line") "definition")'
```

- **`grift`**, grift's own runner, runs a grift program without fux: a file, or
  the program on its stdin, printing only what it prints. On a terminal with
  no file it evaluates each input and prints its result. It comes with
  grift: `cargo install --locked --git
  https://github.com/gold-silver-copper/grift grift-cli`.
- **The dictionary is data.** `(word "NAME")` gives a name's record
  (where it came from, its effect and help, the kinds it can fail with
  and its definition), `(dictionary)` every name's, and `(explain "NAME")`
  what it says of one.
- **A script plays well with the shell.** `(arguments)` gives the arguments
  after the program, `(input)` the rest of stdin, `(read-file PATH)` a file's
  text, `(environment-variable NAME)` a variable (one not set fails
  `unset-variable`), `(warn VALUE)` prints on stderr and `(exit N)` ends with status N.

fuxth, the Forth fux was written in before grift, its `fuxth` runner and fuxed, the
screen editor written in it, are kept in [archived/](archived): fux neither
builds nor runs them.

## Records and events

Every fact fux keeps is a record, which prints as one line of grift data,
`((field value) ...)`, and `get` reads one field of (`fux '(get (pane "sh1") "program")'`):

- `(information (all))` is everything at once; `(information X)` a pane, tab, workspace;
  `(snapshot SCOPE "topology")` the layout alone.
- `(datum (capture PANE))` is a pane's whole observation at one version: its
  screen, version, journal `cursor`, its `program` record (`arguments`, `process-id`,
  state, exit status or signal, `foreground-group`), its terminal's modes
  and marks, hyperlinks and pictures. `(capture PANE)` alone prints the text,
  and `get` reads any field of it.
- `(keys CLIENT)` and `(preferences CLIENT)` are a client's keymap and
  settings, read from that client.
- `(status)` says which server answers, its release and protocol, and its
  `server-process-id`.

The **journal** is the server's numbered events: panes, tabs and
workspaces created, renamed, moved, closed, zoomed and laid out; programs
that hung up, failed to read, exited or respawned; shell commands started
and done; the input and signals requests sent; bells, titles and
notifications; clients attached, shown and detached. A change only to a
screen is no event: it shows as a new version and in captures. `fux
'(events SCOPE (first (arguments)))' CURSOR` prints them as they come, one
record a line, each with its `sequence` and the `next` cursor, until the
scope is gone or the server stops; an empty cursor starts now. Take the
cursor from `information` or from a capture, and you miss nothing between
reading and following. A hook is an ordinary program reading this stream,
and a log is your redirect (`>> FILE`). The journal keeps the last 16,384
events, a size the `journal-entries` setting gives a server as it starts;
a reader past what it still holds fails `journal-loss`.
[docs/records.md](docs/records.md) lists every record and event.

## Configuration

The optional config is `$XDG_CONFIG_HOME/fux/config.grift`, else
`~/.config/fux/config.grift`: grift, which a typing terminal runs as it
attaches. Command lines, `(start)`, watchers and headless clients never
load it. A config builds one candidate client state, keys, definitions
and preferences together; one that fails keeps the whole bundled state
and shows why.

```grift
(move-keys "C-b" "C-a")
(set "shell" (list "/bin/sh" "-l"))
(set "terminal-clipboard" #false)
(define! split-right (lambda! () (split (axis (focused (me)) "horizontal")) #inert))
(bind-key "C-a v" (code! (lambda! () (split-right))))
(bind-key "C-a r l" (code! (lambda! () (move-border (focused (me)) "right" 1))))
(repeat "C-a r")
(bind-key "C-a g" (code! (lambda! () (split (program (axis (focused (me)) "vertical") "top")) #inert)))
(label "top")
(group "Tools")
(bind-key "M-h" (code! (lambda! () (step (me) (focused (me)) "left"))))
```

- **Bindings** run a code value, `(code! (lambda! () ...))`, as your client
  (`(me)`), never a string. `bind-key` takes keys from the root map, each but
  the last a map, made as needed. `move-keys` moves what keys run or hold (here
  the prefix, and every key after it, to `C-a`); `label` and `group`
  change the binding just made, `repeat` makes a map repeat, `unbind`
  takes a key away and `unbind-all` every key. A key bound alone in the
  root map (`M-h`) never reaches a program.
- **`fux '(list-keys)'`** prints the bundled keys as config lines from
  `(unbind-all)` on: `fux '(list-keys)' >> ~/.config/fux/config.grift` gives you
  every default key to edit in place.
- **Settings** are preferences of this client, with their defaults in grift
  ([src/script/client-defaults.grift](src/script/client-defaults.grift));
  `(preferences CLIENT)` gives each one's value and type, and `(set "NAME"
  VALUE)` changes it:

  | Setting | Effect |
  | --- | --- |
  | `shell` | a list: the program, then its arguments, for this client's new panes |
  | `history-bytes` | the history budget in bytes for panes this client creates (`history-budget` changes one pane's) |
  | `journal-entries` | the events the journal of a server this client starts keeps; a running server keeps its own |
  | `reflow` | rewrap recorded primary-screen lines when a new pane is resized |
  | `alternate-scroll-lines` | arrows per alternate-screen wheel action; Ctrl keeps half-page scrolling |
  | `picture-bytes` | the picture budget for new panes, 0 to 96 MiB; zero disables pictures |
  | `escape-delay-ms` | the legacy Escape wait, applied when this client's decoder is idle |
  | `paste-timeout-ms` | the unfinished-paste wait, applied when this client's decoder is idle |
  | `cell-placement` | position after each non-ASCII glyph; bounds drift, not glyph footprints |
  | `program-clipboard` | whether new panes' programs may copy to the terminals' clipboards (OSC 52) |
  | `terminal-clipboard` | whether copies reach this client's terminal clipboard |
  | `bell` | whether this client rings and shows bell marks |
  | `titles` | whether this client's terminal title follows its focused pane |
  | `show-column` | whether the prefix's column is drawn; its keys and the bar's feedback stay |

  `shell`, `history-bytes`, `program-clipboard`, `reflow`, `alternate-scroll-lines`
  and `picture-bytes` supply this client's future launches. `bell`, `titles`,
  `terminal-clipboard`, `cell-placement`, `styles`, `bar`, `bar-slots` and
  `show-column` affect only this client's view. `set` and config evaluation send
  no requests.
- **The bar's place, fields and styles** are settings too.
  `(set "bar" (quote! ("bottom" ("workspace" "tabs") ("pane") #false)))` is
  top, bottom or off, the left fields, the right fields and whether tabs are
  numbered; a field is `"workspace"`, `"tabs"`, `"pane"` or `("slot" N)`, 0 to 15,
  each once. Off, the default, takes no row. `bar-slots` replaces all slots with
  `(N "text")` rows, inert text with no controls, each slot once. `styles` rows are
  `(ROLE FOREGROUND BACKGROUND FLAG...)`: roles `bar`, `tab`, `keys`, `notice`,
  `error`, `panel`, `border`, `border-focus`; colours 0 to 255, `"default"` or
  `"#rrggbb"`; flags `"bold"`, `"dim"`, `"italic"`, `"underline"`. A subset changes
  only the roles it names.
- **Recipe files** hold operations only some want: `(require! "PATH")` reads one,
  relative to the file that names it. A checkout of fux has
  `recipes/layouts.grift` (layout presets) and `recipes/confirm.grift`
  (`confirm`, a question before an action).

## Saved sessions

`(dump (all))` gives what there is as a grift program that makes it again,
and `(dump X)` a tab or a workspace with all in it: the layouts, sizes,
names and the programs each pane started with. Running it on a server,
with `fux --file FILE PARENT`, makes them again in workspace PARENT
(made if missing), else at the top: saving, cloning and moving a project
are that one mechanism (the cookbook's "Save and restore a layout"). A
replay joins a workspace of a name it gives, and refuses a tab or pane
name the server already has (`name-taken`) before it makes anything. A
dump starts new processes: it never brings back what a process held, its
screen, its history or what ran in a shell, and it can repeat a program's
side effects, so read it before you run it.

Nothing is saved on its own: a server that stops takes its session with
it. One that crashes leaves its panes waiting for the next `fux '(start)'` on
its socket, which takes them back with their screens and ids (the
cookbook's "Take a crashed server's panes back"); it keeps its run's own
fux to do it with, so an upgrade waits for the next fresh run. Detaching, or a client's crash, leaves the server and its
programs running. A command line against a server of another fux protocol
runs again with that server's own binary, saying so on stderr; where that
cannot be, it fails `mismatch`, saying why.

## Terminal features

fux writes one terminal dialect: UTF-8, xterm-style cursor movement and
erasing, the alternate screen and SGR colours. It doesn't choose it from
the outer `TERM`; programs in panes get `TERM=xterm-256color`, which
describes fux. What the outer terminal supports, fux asks it, and an
unanswered question stays unknown.

- **Mouse.** fux turns on your terminal's mouse reporting only while the
  focused pane's program asks for it and nothing of fux's is open, and
  gives that program its clicks, drags and wheel. The rest of the time
  your terminal selects and scrolls as it always does. fux itself never
  acts on the mouse.
- **Keyboard.** With a terminal that speaks the kitty keyboard protocol,
  fux tells keys apart that are otherwise sent alike (Shift-Enter and
  Enter), and gives each program keys as it asked: kitty protocol,
  xterm's modifyOtherKeys, or plain.
- **One terminal answers for a tab:** of those showing it, the one that
  last typed into it, else the first attached. Programs asking colours
  (OSC 10, 11, 4), the scheme (mode 2031), sixel and cell size are told
  that terminal's; what it didn't say stays unanswered.
- **Clipboard.** A program that copies with OSC 52 copies to the terminals
  showing it whose `terminal-clipboard` is on, up to 64 KiB. fux keeps no
  copy and never reads the clipboard: paste with your terminal, or from a
  file with `fux '(send-keys (paste (pane 2) (input)))' < FILE`.
- **Bells and notifications** ring and show a brief notice in each
  terminal showing the workspace, at most once every 250 ms; `(set "bell"
  #false)` silences them. **Titles**: with `(set "titles" #true)`, your
  terminal's title is the focused pane's, and is restored when you detach.
- **Pictures** (sixel) a program draws reach your terminal, clipped to
  the pane, when the tab's terminal draws sixel and says its cell size
  (foot, WezTerm, xterm `-ti vt340`, Konsole, mlterm); else the program is
  told there is no sixel, and draws in text.
- **Also:** synchronized output (mode 2026), OSC 8 hyperlinks, underline
  styles, in-band resize reports (mode 2048), and OSC 133 shell marks,
  which count a shell's commands in its pane's version and let
  `last-output` read one's output alone (`fux '(explain "shells")'` has the lines
  that send them, for bash, zsh and fish).

## Left to other programs

| For | Use |
| --- | --- |
| a program's environment | `env(1)`: `(new-tab (argument (argument (program OPTIONS "env") "PORT=8080") "./app"))`; `env -u NAME` removes one |
| a time limit | `timeout(1)`, on a fux command or as a pane's program |
| a file into a pane | `fux '(send-keys (paste (pane 2) (input)))' < FILE` |
| what a pane runs, and where | `ps`, `/proc` and `pgrep` on its program's `foreground-group` (`fux '(explain "agents")'`) |
| stopping a server, whatever it holds | `kill` on its process ([Security](#security)) |
| a log of a pane | `script(1)` as its program, so it still writes to a terminal (`fux '(explain "servers")'`) |
| hooks | an ordinary program reading `events` |
| scrollback, search and copy | your editor, on `C-b c` |
| a format language | grift itself: `get`, `map`, `filter`, and `read-form` on printed data |
| keeping agents apart | a server each, or the OS's isolation: fux has no confinement |

## Security

The server listens only on a Unix socket, which is the only way to
control it. The socket is `FUX_SOCKET`, else `server.sock` in fux's
directory, `$XDG_RUNTIME_DIR/fux` whenever `XDG_RUNTIME_DIR` is set, else
`$TMPDIR/fux`; one of them set empty or to a relative path is refused, as
is neither set. An auto-started server logs to `fux.log` beside it.

- The socket's directory must be a directory of yours, not a symbolic
  link, mode 0700; fux creates the default one so.
- The socket is mode 0600 before any connection is accepted, and a lock
  file makes one server its owner.
- Every peer must run as the server's own user (`SO_PEERCRED` on Linux,
  `getpeereid` on macOS); others, root included, are refused.
- Frames are length-prefixed and at most 1 MiB, checked before anything
  is made for one.

Anyone who can open the socket can run anything as you: as with tmux,
the socket's permissions are the access control. fux has no confinement:
any client may act anywhere on its server. To keep an agent to panes of
its own, give it a server of its own (`fux '(explain "agents")'`); both still run
as you, so neither is kept from your own processes and files.

`kill` on a server's process stops it, whatever it holds, even from its own
panes. `(status)` gives its `server-process-id` where the system says it (Linux);
on macOS, `lsof -t` names it from the socket:

```sh
kill "$(fux '(get (status) "server-process-id")' 2>/dev/null || lsof -t "$FUX_SOCKET")"
```

## Development

CI runs, on Linux and macOS, `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets --locked -- -D warnings`, `cargo test
--workspace --locked` and the rest of [its gates](.github/workflows/ci.yml),
and fux's own tests on Windows.
The tests start real servers and shells: they need `/bin/sh`, `/bin/bash`
and `python3`, use `dash` if it is on `PATH`, and with `FUX_TEST_SHELLS=1`
also zsh and fish. Tools outside the workspace, each with a README:
[`fux-vt/compare`](https://github.com/gold-silver-copper/fux/blob/master/fux-vt/compare/README.md) (fux's terminal emulator
beside other terminals), [`diff`](https://github.com/gold-silver-copper/fux/blob/master/diff/README.md) (fux beside its last
release), [`bench`](https://github.com/gold-silver-copper/fux/blob/master/bench/README.md) (speed, latency and footprint),
[`fuzz`](https://github.com/gold-silver-copper/fux/blob/master/fuzz/README.md) (libFuzzer targets) and [`walk`](https://github.com/gold-silver-copper/fux/blob/master/walk/README.md)
(scripted runs of the real binary).

## License

MIT
