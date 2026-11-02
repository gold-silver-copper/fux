# fux

A minimal terminal multiplexer, driven and configured entirely in
**fuxth**, a small Forth. One server keeps your programs running in
panes, laid out in tabs and workspaces; `fux` attaches a terminal to it,
and several terminals can attach at once, each with its own view and its
own keys. Every other `fux` command line is a short fuxth program, and so
are fux's config, key bindings, command prompt and saved sessions.

fux is made for agents first, and the same words serve people. A program
driving fux reads every fact as a record; learns every multiplexer and
program-lifecycle change from a journal of events, and every screen
change from versions and captures; gets live process facts from the OS;
and acts only on the screen it saw. fux reports facts (a version moved,
a program exited with 7, a text is on the screen) and never judges
whether a program is ready, idle or done. What the OS already does, the
OS does: `env`, `timeout`, `kill`, `ps`, your terminal's clipboard and
your editor.

fux is one binary for macOS and Linux, built with Rust 1.95 or later:
`cargo install --locked fux`, or in a clone `cargo install --locked --path
.`. `fux` alone attaches, starting a server if none runs. The `fuxth`
runner is a program of its own: `cargo install --locked --path fuxth-run`.

- `fux help agents` is the page for driving programs from a script or an
  agent ([docs/agents.md](docs/agents.md)), `fux help TOPIC` the rest.
- `fux help cookbook` has short recipes to copy, each run by fux's tests
  ([docs/cookbook.md](docs/cookbook.md)).
- `fux help` lists every word, `fux help WORD` says one at length, and
  `fux help json` gives every record ([docs/json.md](docs/json.md)).

## Using it

`fux` attaches to the server, starting one in the background if none
answers, and `C-b d` detaches: the programs keep running. `fux opts str
work in attach` attaches to workspace `work`, `fux @2 attach` shows a tab,
and `fux %3 attach` focuses a pane; each must exist. `fux opts watch
attach` only watches: its keys do nothing but `C-b d`, and it never
starts a server. Inside a fux pane, `fux` refuses to attach.

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
  **stacks**: `PANE stack` puts a new pane on that pane's rectangle, as
  `split` puts one beside it, and each client is shown one child of a
  stack, its own. Every child keeps its size, so choosing one resizes no
  program. In a stack, `C-b o` shows the next child, `C-b q` the last.
- **The bar** is hidden: panes have every row. While the prefix, or any
  screen of fux's, is in use, the bar draws over the bottom row, with the
  workspace's path and its tabs on the left and the focused pane or what
  the keys do on the right; when it ends, the row shows the pane again. A
  bell or a program's notification (OSC 9, 777) shows in the bottom-right
  corner for five seconds, or until your next key. A screen too small for
  its tab shows the panes cut off, and the corner names them (`[hidden %3]
  [cut %2]`). No pane is ever resized for fux's own feedback. To give the
  bar a row of its own, see the cookbook's "The bar on a row of its own".

The server runs until its last pane closes, until `fux kill-server`
(refused from its own panes and while a person's terminal is attached), or
until SIGTERM, SIGINT or SIGHUP, which `kill` sends ([Security](#security)
shows how on Linux and macOS). It then hangs up every pane, tells each
attached terminal why, and removes its socket.
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
| `h` `j` `k` `l` | `me-focused str left step` … | focus the pane that way |
| `o` / `q` | `me-focused str next step` / `me-focused str last step` | focus the next / last pane (in a stack, child) |
| `v` / `s` | `me focused str horizontal axis split drop` / `… str vertical axis …` | split side by side / stacked |
| `x` | `me-focused confirm-close` | close the pane (`y` or Enter closes, `n` or Esc keeps it) |
| `z` | `me focused zoom` | zoom the pane, or restore it, on every screen showing its tab |
| `c` | `me edit-history` | the pane's history in your `$EDITOR` |
| `a`, then a key | `me-focused rename-prompt` … | pane actions: `r` rename, `x` close, `k` terminate the running command, `s` swap, `t`/`w` move to a tab/workspace, `n`/`W` move to a new one, `m h`/`m l` reorder |
| `n` / `b` | `me-focused tab str next step` / `… str previous step` | next / previous tab |
| `t`, `w` | the tab and workspace maps | `n` new, `h`/`l` previous and next, `g` choose, `r` rename, `x` close, `m h`/`m l` move |
| `e` | `me command-prompt` | type any fuxth |
| `d` | `me detach` | detach this terminal |
| `r`, then `h` `j` `k` `l` | `me focused str left 1 move-border` … | move a border one cell |
| `m`, then `h` `j` `k` `l` | `me focused str left move` … | move the pane beside its neighbour |

`r` and `m` are maps that **repeat**: `h` `j` `k` `l` act again without
the prefix until Esc or Enter. `f`, `g`, `i`, `u` and `y` are free for
your own. `me-focused` is the keys' own word for `me me focused`, the
client and the pane it focuses; `str last step` toggles with the one this
client was shown before.

- **`C-b c`** opens the focused pane's history in your editor (the
  attached terminal's `$EDITOR`, else `vi`) at its last line, in a split
  titled `history` that closes when the editor exits: search, select and
  copy there. It stands in for a copy mode and a pager.
- **Choosers** (`t g`, `w g`, `a s`, `a t`, `a w`) list tabs, workspaces
  or panes: Enter or `y` selects, `r` renames, `x` closes, Esc, `q` or
  `n` cancels. They are library words over one `choose` primitive
  (`choose-among`, `choose-move`, `choose-pane`; `fux help choose`).
- **The command prompt** (`e`) runs fuxth as your client, `me` and all,
  and shows what it printed, or its failure, in the bar. `prompt` opens
  a line of your own whose Enter runs your code with the line.
- **A key's program** runs as your client, one at a time; while one runs
  that may open a screen, the keys you type wait for it. The prefix, then
  Esc, stops one that takes long.

The keys are a config, run before yours, in the words yours uses
([src/script/keys.fux](src/script/keys.fux)): `fux list-keys` prints them
as config lines ([Configuration](#configuration)).

## Driving it

A command line is a fuxth program: its words run left to right, each
taking its values from a stack and leaving its own. `%3`, `@2` and `+1`
are a pane, a tab and a workspace, and `str NAME pane` finds a pane by
its name; `str WORD` pushes any text. What is left is printed, a value a
line: a capture as its text, a record as one line of JSON.

```sh
fux opts str agent in 80 24 size str sh program new-tab str sh1 rename
A=$(fux str sh1 pane str 'sleep 1; echo "six times seven: $((6*7))"' line)
fux str "$A" changed wait
fux str sh1 pane str 'seven: 42' 30 shows
fux str sh1 pane capture json | jq -c '{version, cursor, program}'
fux ls listing
```

The first line starts a shell in a new tab in workspace `agent`, made if
it isn't there, and names its pane `sh1`; from outside fux a new tab
needs a size, as fux makes none up. What a word that acts takes (the
program, keys, a size) goes before it, on `opts` or on the pane it acts
on.

Every pane has a **version**, one word that names the pane, counts what
only grows while its program runs (respawns, its screen's revision,
bells, shell commands ended, its end) and names the server's run. An
action (`new-tab`, `line`, `send-keys`, `signal` …) acts at once and
answers its pane's version from just before it acted; `changed wait`
waits until the version differs, and says what changed. `shows`, `quiet`
and `until-ended` wait for a text, for a quiet screen and for the
program's end, and `expect` sends keys only if the pane is still at the
version you read. fux never judges whether a program is done: `fux help
agents` says what to wait for, and `fux help recipes` how.

A failure prints nothing more on stdout: stderr says `fux: CODE:
MESSAGE`, with the file, line and word, then what was left. fux exits 1
for a refusal; 2 for a mistake in the program (an unknown word, a value of
the wrong type …); 4 if the pane waited on closed; 124 if nothing changed
in time; and 141, quietly, if stdout's reader went (`| head -n 1`). A
single argument is a whole program (`fux 'ls json'`), never a file; `fux
--file FILE ARGS…` runs a file, so a script can start with `#!/usr/bin/env
-S fux --file`, and `fux -` runs what stdin says.

A handle names everything under it: `info`, `dump`, `events`, `panes`,
`tabs` and `kill` take a pane, a tab, a workspace or `all`, and apply to
the whole of it, so `kill` on a project closes its agents' workspaces too.

```sh
fux opts str proj in str agent1 in 80 24 size str sh program new-tab > /dev/null
fux opts str proj in str agent2 in 80 24 size str sh program new-tab > /dev/null
fux str proj workspace panes len
fux str proj workspace kill
```

Names are given, never made: `name` and `rename` give one, and a
workspace, tab or pane that has a name refuses it to another
(`name_taken`), so a name finds one thing. fux sets no limits of its own
on memory, time or steps: `ulimit`, cgroups and `timeout` do.

## fuxth

fuxth is a small Forth with full English words, so a program needs no
quoting beyond what the shell needs: `define … end` makes a word, `if …
else … then` and `begin … until` decide and repeat, `str timeout try …
catch … end` handles a failure. It has a dictionary, immediate and defining
words, a data space of memory-safe cells and execution tokens. Words run
as they are read and each checks its values as it runs, so a failure
comes after the words before it have acted, and stderr lists what took
effect. `fux help` lists the core's words and the libraries', which are
written in fuxth themselves (`fux help WORD` shows each one's code);
[fuxth/README.md](fuxth/README.md) is the language at length.

```sh
fux 'define double 2 mul end 21 double'
echo 'define square dup mul end 7 square' | fuxth
fux str line word str definition get
```

- **`fuxth`** runs a fuxth program without fux: a file, or the program on
  its stdin, printing only what it prints. On a terminal with no file it
  is a REPL.
- **fuxed** is a small screen editor written in fuxth, on the `fuxth`
  runner: `fuxth fuxed/fuxed.fux FILE` in a checkout of fux
  ([fuxed/README.md](fuxed/README.md)).
- **The dictionary is data.** `str NAME word` gives a word's record
  (where it came from, its effect and help, the codes it can fail with
  and the words that made it), `dictionary` every word's in the order
  they were defined, `see NAME` what made it and `words` every name.
- **A script plays well with the shell.** `args` gives the arguments
  after `--file FILE`, `input` the rest of stdin, `read-file` a file's
  text, `getenv` a variable (one not set fails `unset_var`), `warn` prints
  on stderr and `N exit` ends with status N.

## Records and events

Every fact fux keeps is a record, which `json` prints as one line of JSON
and `get` reads one field of (`fux str sh1 pane str program get json`):

- `ls` is everything at once; `X info` a pane, tab, workspace or `all`;
  `SCOPE str topology snapshot` the layout alone.
- `PANE capture json` is a pane's whole observation at one version: its
  screen, version, journal `cursor`, its `program` record (argv, `pid`,
  state, exit status or signal, `foreground_group`), its terminal's modes
  and marks, hyperlinks and pictures.
- `CLIENT keys` and `CLIENT preferences` are a client's keymap and
  settings, read from that client.
- `status` says which server answers, its release and protocol, and its
  `server_pid`.

The **journal** is the server's numbered events: panes, tabs and
workspaces created, renamed, moved, closed, zoomed and laid out; programs
that hung up, failed to read, exited or respawned; shell commands started
and done; the input and signals requests sent; bells, titles and
notifications; clients attached, shown and detached. A change only to a
screen is no event: it shows as a new version and in captures. `SCOPE str
CURSOR events` prints them as they come, one JSON object a line, each with
its `seq` and the `next` cursor, until the scope is gone or the server
stops; an empty cursor starts now. Take the cursor from `ls` or from a
capture, and you miss nothing between reading and following. A hook is an
ordinary program reading this stream, and a log is your redirect (`>>
FILE`). The journal keeps the last 16,384 events, a size the
`journal-entries` setting gives a server as it starts; reading past what
it still holds fails `missed`. `fux help json` lists every record and
event ([docs/json.md](docs/json.md)).

## Configuration

The optional config is `$XDG_CONFIG_HOME/fux/config.fux`, else
`~/.config/fux/config.fux`: fuxth, which a typing terminal runs as it
attaches. Command lines, `fux start`, watchers and headless clients never
load it. A config builds one candidate client state, keys, definitions
and preferences together; one that fails keeps the whole bundled state
and shows why.

```fux
str C-b str C-a move-keys
str shell list str /bin/sh str -l end set
str terminal-clipboard false set
define split-right me focused str horizontal axis split drop end
str 'C-a v' code split-right end bind
str 'C-a r l' code me focused str right 1 move-border end bind
str 'C-a r' repeat
str 'C-a g' code me focused str vertical axis str top program split drop end bind
  str top label  str Tools group
str M-h code me-focused str left step end bind
```

- **Bindings** run a code value, `code … end`, as your client (`me`),
  never a string. `bind` takes keys from the root map, each but the last
  a map, made as needed. `move-keys` moves what keys run or hold (here
  the prefix, and every key after it, to `C-a`); `label` and `group`
  change the binding just made, `repeat` makes a map repeat, `unbind`
  takes a key away and `unbind-all` every key. A key bound alone in the
  root map (`M-h`) never reaches a program.
- **`fux list-keys`** prints the bundled keys as config lines from
  `unbind-all` on: `fux list-keys >> ~/.config/fux/config.fux` gives you
  every default key to edit in place.
- **Settings** (`fux help set` lists them): the shell, history and
  picture budgets, reflow, the bell, titles, the terminal clipboard, the
  journal's size and the decoder's timings. Each is a preference of this
  client, with its default in fuxth ([src/script/client-defaults.fux](src/script/client-defaults.fux)).
- **The bar's place, fields and styles** are settings too: `str bar …
  set`, `str bar-slots …`, `str styles …` (`fux help set`).
- **Recipe files** hold words only some want: `require PATH` reads one,
  relative to the file that names it. A checkout of fux has
  `recipes/layouts.fux` (layout presets) and `recipes/confirm.fux`
  (`confirm`, a question before an action).

## Saved sessions

`all dump` prints what there is as a fuxth script that makes it again,
and `X dump` a tab or a workspace with all in it: the layouts, sizes,
names and the programs each pane started with. Running it on a server,
with `fux --file FILE PARENT`, makes them again in workspace PARENT
(made if missing), else at the top: saving, cloning and moving a project
are that one mechanism (the cookbook's "Save and restore a layout"). A
replay joins a workspace of a name it gives, and refuses a tab or pane
name the server already has (`name_taken`) before it makes anything. A
dump starts new processes: it never brings back what a process held, its
screen, its history or what ran in a shell, and it can repeat a program's
side effects, so read it before you run it.

Nothing is saved on its own: a server that stops or crashes takes its
session with it. Detaching, or a client's crash, leaves the server and its
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
  file with `input paste send-keys < FILE`.
- **Bells and notifications** ring and show a brief notice in each
  terminal showing the workspace, at most once every 250 ms; `str bell
  false set` silences them. **Titles**: with `str titles true set`, your
  terminal's title is the focused pane's, and is restored when you detach.
- **Pictures** (sixel) a program draws reach your terminal, clipped to
  the pane, when the tab's terminal draws sixel and says its cell size
  (foot, WezTerm, xterm `-ti vt340`, Konsole, mlterm); else the program is
  told there is no sixel, and draws in text.
- **Also:** synchronized output (mode 2026), OSC 8 hyperlinks, underline
  styles, in-band resize reports (mode 2048), and OSC 133 shell marks,
  which count a shell's commands in its pane's version and let
  `last-output` read one's output alone (`fux help shells` has the lines
  that send them, for bash, zsh and fish).

## Left to other programs

| For | Use |
| --- | --- |
| a program's environment | `env(1)`: `str env program str PORT=8080 arg str ./app arg new-tab`; `env -u NAME` removes one |
| a time limit | `timeout(1)`, on a fux command or as a pane's program |
| a file into a pane | `fux %2 input paste send-keys < FILE` |
| what a pane runs, and where | `ps`, `/proc` and `pgrep` on its program's `foreground_group` (`fux help agents`) |
| stopping a server, whatever it holds | `kill` on its process ([Security](#security)) |
| a log of a pane | `script(1)` as its program, so it still writes to a terminal (`fux help servers`) |
| hooks | an ordinary program reading `events` |
| scrollback, search and copy | your editor, on `C-b c` |
| a format language | `json` with `jq`, or `X str FIELD get` |
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
its own, give it a server of its own (`fux help agents`); both still run
as you, so neither is kept from your own processes and files.

`kill` on a server's process stops it, whatever it holds, even from its own
panes. `status` gives its `server_pid` where the system says it (Linux);
on macOS, `lsof -t` names it from the socket:

```sh
kill "$(fux status str server_pid get 2>/dev/null || lsof -t "$FUX_SOCKET")"
```

## Development

CI runs, on Linux and macOS, `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets --locked -- -D warnings`, `cargo test
--workspace --locked` and the rest of [its gates](.github/workflows/ci.yml).
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
