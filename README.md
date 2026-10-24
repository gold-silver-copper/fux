# fux

A small terminal multiplexer. One server keeps your shells running in
workspaces, tabs and split panes; `fux` attaches a terminal to it. Every
other `fux` command line is a short program in **fuxth**, a tiny Forth made
for the shell, and so are fux's config, key bindings, command prompt and
saved sessions. Several terminals can attach at once, each with its own
view and its own keys. fux is driven from the keyboard: it has no mouse
actions of its own, by design, and passes the mouse only to a program that
asks for it (see [Terminal features](#terminal-features)).

fux is one binary for macOS and Linux, built with Rust 1.95 or later.

```sh
cargo install --locked fux        # or, in a clone: cargo install --locked --path .
fux                               # attach, starting a server if none runs
```

## Using it

`fux` alone attaches this terminal, starting a server in the background if
none answers; `fux str work attach-to` attaches to workspace `work`, and
`fux watch` only watches (keys do nothing but `C-b d`, which detaches). A
new server has one workspace, tab and shell. `C-b d` detaches, and the
shells keep running. Inside a fux pane, `fux` refuses to attach.

The server runs until `fux kill-server`, SIGTERM, SIGINT, SIGHUP, or until
its last pane closes. It then hangs up every pane, tells each attached
terminal why, and removes its socket.

Every pane runs your shell (`str shell str /bin/zsh set`, else `$SHELL`,
else `/bin/sh`). A pane closes when its shell exits, showing its viewers the
exit status; a tab closes with its last pane, a workspace with its last tab.
A pane's lines re-wrap when its width changes, history included.

The bottom row is the bar: the workspace and its tabs on the left; on the
right the focused pane's number and title (the program's OSC 0/2 title,
else the pane's name), a notice, or copy mode's position.

## fuxth

A fux command line is a program: words run left to right, each taking its
values from a stack and leaving its own.

```sh
fux ls                                            # what there is
fux opts str htop program run                     # start htop: %3 0.1.0.0.0, its pane and version
fux %3 str 0.1.0.0.0 changed wait                 # until its version differs: %3 0.4.0.0.0 screen
fux %3 capture                                    # its screen
fux %3 str q text send-keys                       # press q: %3 0.9.0.0.0, its version before
```

- A number is a value; `%3`, `@2` and `+1` alone are a pane, a tab and a
  workspace; any other bare word is a word to run. `str WORD` pushes WORD as
  text, whatever it looks like: `str 'two words'`.
- What is left at the end is printed, a value a line: a pane as `%3`, a tab
  `@2`, a workspace `+1`, a client `c1`, a capture or listing as its text.
  `json` turns a value into JSON: `fux ls json`, `fux str %3 pane info json`.
- Every pane has a **version**, `STARTS.SCREEN.BELLS.DONE.ENDED`: counts
  of its program's starts, its screen's revisions (output read, resizes to another size,
  history cleared or resized), bells rung and shell commands ended (OSC
  133 marks), and 1 once its program ended; they only grow while one
  program runs (`respawn` starts all but the first afresh, a server
  restart all of them). An action (`run`,
  `send-keys`, `signal` …) acts at once and answers its pane and its
  version from just before it acted. `PANE str V changed wait` waits until
  the version differs from V or the pane closes, and answers what changed:
  `%3 0.57.1.4.0 screen bell done`, `exited 2`. fux never judges
  whether a program is done: [docs/agents.md](docs/agents.md) has recipes
  (a text, quiet for N seconds, a command's end, an exit). A failure
  prints `fux: CODE: MESSAGE` on stderr, with the file, line and word, and
  exits 1; a wait exits 4 if the pane closed, 124 if nothing changed in
  time (`SECS timeout` on the pane; 30 s by default).
- A single argument with spaces is a whole program (`fux 'ls json'`);
  otherwise `fux FILE` runs a file (one with a `/` or ending `.fux`), and
  `fux -` runs what stdin says. `#` starts a comment. A paste is at most 900 KiB.
- The language's core is small on purpose: 21 words (`drop pick roll`, `add
  sub mul div`, `lt eq`, `len at slice concat push put`, `code char`, `kind
  string print fail`) and its parsing words: `str WORD`, `define NAME …
  end` (a word of your own; it may call itself, and a call in last place
  loops in one frame), `X let NAME` (NAME pushes X from then on), `if …
  else … then`, `begin … until`, `list … end` (a list of what its body
  leaves) and `SEQ each … end` (its body once per element of a string or
  list). Values are integers (checked: an overflow fails; `div` truncates
  toward zero), booleans, strings, lists, and fux's own (panes, options …).
  A word may call one defined after it in the same file, and a definition
  may take a library word's name (from there on; the core's words are kept).
- Everything else is the standard library, written in fuxth: `dup swap over
  rot`, `not and or gt min max mod abs`, text (`words lines split-on trim
  upper lower number join contains starts-with pad`), lists (`range sort
  reverse sum count first last pack`) and more; `fux help` lists them all.
- A program is checked before it runs: a word given fewer values than the
  stack surely holds, a value the core's words never take (`str a 1 add`,
  `1 if`), or keys or options left that no word acts on (`%3 str x text`),
  fails before any word acts. What can't be known before it runs (a
  library word's values, a loop, a list's elements) is checked as it runs.
- No word is a character a shell treats specially, so a program needs no
  quoting beyond the text you give `str`.

`fux help` lists every word with what it takes and leaves (`x secs -- x`);
`fux help WORD` (`fux str WORD explain`) says one at length, with
its options and an example;
`fux help-agents` is the recipe for scripts and agents, its first screen
enough for most work and `fux help agents TOPIC` the rest
([docs/agents.md](docs/agents.md)), and `fux help-json` what `json` gives
([docs/json.md](docs/json.md)).

What a word that acts takes (**options**, a wait, keys, a mouse event) goes
on the pane before it, in any order: the pane given them becomes a request,
which the word at the end acts on. `run`, `new-tab` and `new-workspace` have
no pane yet: they start from `opts`, empty options. Only `run` takes a program
(`command`, `program`): `new-tab` and `new-workspace` start a shell, and
`str 'make test' typed` types a line into it.

```sh
fux opts str web name 120 40 size str './app --verbose' command run
fux %3 str 0.57.0.3.0 changed 60 timeout wait
fux %3 200 history capture
fux %3 str 'make test' text str Enter key send-keys
```

A program can name what it made; a wait on an action's answer waits from the version it holds:

```sh
fux opts str 'sh -c "echo hello; sleep 1"' command run let P   P wait drop   P screen
```

The bundled words are written in fuxth themselves: `type` and `press` (text
and a key into a pane), `line` (a line typed and Enter), `screen` (a capture) and `shell` (a tab with a shell in
workspace `agent`).

### Finding things

A pane is `%N`, a tab `@N`, a workspace `+N` or its name, a client `cN`:
`str %3 pane`, `str @2 tab`, `str work workspace`, `str c1 client`. `pane`
also takes a tab's name (`run`'s tabs are named after their program), `tab`
and `workspace` a pane, the tab or workspace it is in. Inside a pane,
`here` is that pane (`FUX_PANE`, below). In a key's program or the
prompt's, `me` is the client whose key it is, and `me focused` its focused
pane.

### Which server

`str NAME server` before the rest is the server named NAME (its socket
`NAME.sock` in `$XDG_RUNTIME_DIR/fux`), `str PATH socket` a socket's path;
without either, `FUX_SOCKET`, else the default. A private server for a test
or an agent:

```sh
fux str test server start                         # returns once it answers
fux str test server opts str ./app program run
fux str test server kill-server
```

## Keys

Every key below follows the prefix, `C-b` by default, and is a lower-case
letter but `a W`: case counts, so `C-b T` is not `C-b t`. The keys are your client's:
each terminal attached has its own prefix and bindings, from its config, and
the server knows none of them. A key that is not fux's reaches the focused
pane as you typed it. So are fux's screens: the command column,
choosers, prompts, confirmations and copy mode belong to your client, which
switches between them at the key that does it, so a key typed fast is never
read in the wrong one; the server draws what your client shows, and keeps
copy mode's cursor in the history only it holds.

The prefix alone opens the **command column**, listing every binding, and
those without the prefix last: arrows, PageUp/PageDown and Home/End move,
Enter runs the selected one, a bound key runs its own, Esc closes. A key you
bind is yours there, even one the column moves with; Esc always closes. The
prefix again sends it to the pane.

| Key | Runs | Does |
| --- | --- | --- |
| `h` `j` `k` `l` | `me str left focus` … | focus the pane that way |
| `o` / `q` | `me str next focus` / `me str last focus` | focus the next / last pane |
| `v` / `s` | `me focused horizontal split drop` / `… vertical …` | split side by side / stacked |
| `x` | `me me focused confirm-close` | close the pane (asks `y`/`n`) |
| `z` | `me zoom` | zoom the pane, or restore it |
| `a`, then a key | `me me focused rename-prompt` … | pane actions: `r` rename, `x` close, `k` terminate the running command, `s` swap, `t`/`w` move to a tab/workspace, `n`/`W` move to a new one, `m h`/`m l` reorder |
| `c` | `me copy-mode` | copy and select |
| `p` | `me focused 0 paste-buffer` | paste the newest copy |
| `n` / `b` | `me next-tab` / `me previous-tab` | next / previous tab |
| `e` | `me command-prompt` | type any fuxth |
| `d` | `me detach` | detach this terminal |
| `r`, then `h` `j` `k` `l` | `me focused str left 1 move-border` … | move a border one cell |
| `m`, then `h` `j` `k` `l` | `me focused str left move-beside` … | move the pane beside its neighbour |
| `t`, `w` | tab and workspace layers | below |

`r` and `m` are **repeat modes**: `h` `j` `k` `l` act again without the
prefix until Esc or Enter (as `h` and `l` do after `a m`, `t m` and `w m`);
the prefix ends the mode and opens the column; any other key ends it and
is dropped, which the bar says.

`a`, `t` and `w` are **layers**. `t` and `w` share their verbs: `n` new,
`h`/`l` previous and next, `g` choose, `r` rename, `x` close, and `m` then
`h`/`l` to move it earlier or later, repeating. A layer is bindings like any
other: `str 'a g' str '…' bind` adds one to it, listed in the column beside
the defaults, and `unbind` takes one away. `f`, `g`, `i`, `u` and `y`
are free for your own bindings. These keys are themselves a config, run
before yours, in the words yours uses
([src/script/keys.fux](src/script/keys.fux)); `fux list-keys` prints yours
the same way.

- **Choosers** (`t g`, `w g`, `a s`, `a t`, `a w`) list tabs, workspaces
  or panes. A chooser acts on the item it was opened for, even once
  renamed, and closes if that item is gone.
  Enter selects, `r` renames, `x` closes, Esc or `q` cancels; arrows,
  `j`/`k`, PageUp/PageDown and Home/End move.
- **The command prompt** (`e`) runs fuxth as your client, `me` and all
  (`me focused vertical str htop program split`), and shows what it
  printed, or its failure, in the bar. It and the rename prompts edit with
  the arrows, Home, End, Backspace and Delete.
- **A key's program** runs as your client, one at a time. While one that may
  open a screen runs (it has `copy-mode`, a chooser or another such word), the
  keys you type wait for it, and go to the screen it opens. The prefix, then
  Esc, stops a program a key or the prompt runs, if it takes long.

Directional focus picks, among the panes beyond the focused pane's edge,
the one whose centre is closest across the direction, then along it, then
the lowest number.

### Copy and select

`C-b c` puts a keyboard cursor on the focused pane. For your terminal the
pane holds still while output continues; other terminals see it live. If
the history drops the rows copy mode holds, copy mode ends and says why.

Keys are letters, in either case, and the brackets; the arrows,
PageUp/PageDown, Home, End, Enter and Esc also work.

| Keys | Do |
| --- | --- |
| `h` `j` `k` `l`, arrows | move |
| `w` / `b` | next word / back a word |
| `a` / `e` or End | first / last non-blank of the line |
| Home | column 0 |
| `u` / `d` | half a page up / down |
| PageUp / PageDown | a page up / down |
| `t` / `z` | top of the history / the live bottom |
| `f` / `r`, then `n` / `p` | search forward / back; next / previous match |
| `[` / `]` | previous / next shell prompt |
| `v` / `s` / `x` | select characters / lines / a block; again to clear |
| `o` | swap the selection's ends |
| `y` or Enter | copy and leave |
| `q` or Esc | leave without copying |

Search is literal over the whole history, and ignores case unless the
query has a capital letter. `[` and `]` find prompts marked with
`OSC 133 ; A` (see [Shell marks](#shell-marks)). A copy keeps wide
characters whole, joins soft-wrapped lines, and trims trailing blanks. One
copy is at most 262,144 cells.

**Clipboards.** A copy goes to fux's paste buffers (the newest 16,
`buffers`); `C-b p` pastes the newest, bracketed if the program asked for
bracketed paste. It is also sent to your terminal's clipboard as OSC 52, up
to 1 MiB encoded, unless `str clipboard str off set`; your terminal must
allow OSC 52 writes. A program in a pane that copies with OSC 52, as Neovim
and Helix do, copies the same way. fux never reads the clipboard.

## Shell marks

fux starts your shell as it is, and reads the OSC 133 marks any program
sends: `C` where a command's output begins, `D;STATUS` where it ends, `A`
where a prompt starts. With them, a pane's version counts the commands
that end, `last_status` says how the last did, `last-output` reads its
output alone, and copy mode's `[` and `]` jump between prompts; without
them, each says there are none. To send them, in
your own startup files:

```sh
# bash 4.4 or later (macOS's /bin/bash 3.2 has no PS0): ~/.bashrc
PS0='\e]133;C\a'
PROMPT_COMMAND='printf "\e]133;D;%s\a\e]133;A\a" "$?"'${PROMPT_COMMAND:+;$PROMPT_COMMAND}
```

```zsh
# zsh: ~/.zshrc
autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
precmd_functions=(mark_end $precmd_functions)  # first, so $? is the command's
```

```fish
# fish: ~/.config/fish/config.fish
function mark_start --on-event fish_preexec; printf '\e]133;C\a'; end
function mark_end --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
function mark_prompt --on-event fish_prompt; printf '\e]133;A\a'; end
```

## Words

`fux help` is the full list. In short, with what each takes and leaves:

| Words | Do |
| --- | --- |
| `opts … run` (`-- pane`) | start a program in a tab of its own in workspace `agent` (`str WS in` elsewhere), no client moved to it; its pane stays once it exits, with its last screen and status. Options: `name`, `size` (COLS ROWS), `cwd`, `env` (KEY VALUE, a pane, tab or workspace as its id: `%3`), `program`, `arg`, `log-to`; answers the pane and its version as it starts |
| `pane … split`, `opts … new-tab`, `opts … new-workspace` | a shell beside a pane (or with `program`, that program), in a new tab (`str WS in`: a workspace, made if missing) or a new workspace; `horizontal`/`vertical`, `typed LINE` types a line into the new shell. Names are unique, and do not start with `+`, `%` or `@`. An option a word does not use is refused |
| `pane … respawn` | start again in its pane what `run` started, its id and size kept; `kill-first` ends it first |
| `pane … wait` | wait until its version differs from `str V changed` (else from the one an action's or wait's answer holds, else from now's) or the pane closes, and answer what changed; `SECS timeout` (else `wait-seconds`, 30). On a tab or workspace, any of the panes there as it begins |
| `pane … send-keys` | type `str T text`, keys by name (`str C-c key`), `str T paste` or `str FILE paste-file` (bracketed if the program asked), bytes as they are (`str 1b5b bytes`, in hex: part of a key's sequence or of a character); `N times` repeats the input just before it; answers at once, the pane and its version before the keys. To a tab: each of its panes. To a client (`str c1 client`): as typed at its terminal, its bindings included |
| `pane str ACTION ROW COL mouse send-mouse` | a mouse event at a cell, from 0 (`click right middle press release wheel-up wheel-down move`), encoded as the program asked |
| `pane … capture` | the pane's text, trimmed, a line wrapped at the pane's width joined whole (`wrapped`: each screen row a line); `N history`, `all-history`, `full`, `styles`, `marks` (reverse video and background colours as `⟦this⟧`), `main-screen`, `alt-screen`, `last-output`, `A B rows`, `A B cols` |
| `ls`, `X info`, `X str FIELD get` | everything; one pane, tab or workspace; one field of its JSON, as a number, boolean or text |
| `events`, `X events-of` (and `-json`) | what happens, a line each as it happens: made, closed, moved, renamed; a program's exit, respawn, title, size, bells, notifications; a shell command's end |
| `X kill`, `X str NAME rename`, `X str TEXT notify` | close (without asking), rename, a notice on the clients showing it |
| `X COLS ROWS resize`, `X resize-auto` | what a tab is laid out on while no client shows it |
| `pane N resize-x`, `pane N percent resize-y`, `pane str WAY N move-border` | a pane's width or height in cells or a percent; move its nearest border |
| `pane X move-to`, `pane str WAY move-beside`, `pane pane swap-panes`, `X str next reorder` | move a pane to a tab, workspace, `new-tab` or `new-workspace`; beside a neighbour; swap; move one place |
| `pane str TERM signal` | a signal to what runs in the pane's terminal's foreground, never the shell of a shell pane |
| `pane str FILE start-log`, `pane stop-log`, `pane clear-history` | keep what its program writes from now on; stop; drop its history |
| `buffers`, `N buffer`, `pane N paste-buffer` | paste buffers, newest 0 |
| `dump`, `X dump-of` | what there is, as a program that makes it again |
| `str WS str PATH grant`, `str PATH revoke` | a socket whose requests reach workspace WS alone (see [Security](#security)) |
| `client …` | `focus`, `focus-pane`, `show-tab`, `next-tab`, `show-workspace`, `choose-tab`, `rename-prompt`, `confirm-close`, `zoom`, `copy-mode`, `command-prompt`, `detach`, `capture-client`, `resize-client` act on a client's screen |
| `COLS ROWS headless` | a client with no terminal, for tests and scripts: prints its id, ends on a signal |
| `start`, `status`, `servers`, `kill-server`, `force-kill-server`, `version` | the server |

**Key names**, for `key`, `bind` and `prefix`, are tmux's (`C-x`, `M-x`,
`S-Left`, `Enter`, `BTab`, `BSpace`, `PageUp`, `F1`–`F35`, …) and any
single character, which carries its own shift (`T`, not `S-t`); common
spellings (`Backspace`, `Esc`, `Return`, `Del`, `Ins`, `PgUp`, `PgDn`) are
taken too, and a name refused is told the closest one. `fux key-names`
prints them all.

**Inside a pane**, a program's environment holds:

| Variable | Value |
| --- | --- |
| `TERM` | `xterm-256color` |
| `TERM_PROGRAM`, `TERM_PROGRAM_VERSION` | `fux` and fux's version, not the outer terminal's, whose features fux may not have |
| `FUX_PANE` | the pane, `%N`: `here` to a fux command run there |
| `FUX_SOCKET` | the socket of the pane's server, so a fux command there reaches it; a grant's socket if the pane was started through one, so what runs there stays within the grant |
| `FUX_SERVER` | which run of which server the pane is of. A fux command from a pane of another server, or of an earlier run of this one, has no `here` there (`stray_pane`); `kill-server` from one of the server's own panes is refused (`own_server`) |
| `FUX` | the fux executable the server runs, unset if the server cannot name it: what a key's program or a script in a pane runs, as `"${FUX:-fux}"`, when `fux` may not be on its PATH (a server started from a desktop launcher or a systemd unit) |

The rest is the server's own environment, as it was when the server
started. Two things override it and the table alike: the names in
`update-environment`, which a pane takes from whoever starts it, and, last,
what a program's `env` sets (`opts str NAME str VALUE env str PROGRAM
program run`). A name in `update-environment` that the starter lacks is
unset, but `FUX`, `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER` then keep the
table's value. A starter with no environment at all, as `env -i` leaves
one, changes none.

XTVERSION (`CSI > q`) answers `fux` and its version, secondary device
attributes (`CSI > c`) the version, and primary ones (`CSI c`) a
VT220-class terminal.

### JSON

`json` on what a word left gives its JSON: `ls json`, `X info json`,
`pane capture json`, `client capture-client json`, an action's or a
wait's answer (`… wait json`), `status json`, `servers json`; `events-json` and
`events-of-json` stream an object a line. These shapes are part of fux's
interface; `fux help-json` lists every one ([docs/json.md](docs/json.md)).

```json
{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main",
  "panes":[{"id":"%1","name":"zsh","title":"","rows":23,"cols":80,"pid":4242,
    "version":"0.57.0.3.0","program":"vim","paused":false,"exited":null,"signal":null,"idle_ms":1520,"alt_screen":true,
    "mouse":true,"mouse_mode":"press_release","bracketed_paste":true,"pictures":[],"cursor_visible":true,
    "history_lines":0,"copy_mode":false,"command_running":false,
    "last_status":null,"bells":0,"notification":null,"cwd":"/home/me",
    "argv":["vim","notes.md"]}]}]}],
 "clients":[{"id":"c1","rows":24,"cols":80,"workspace":"+1","tab":"@1","pane":"%1","zoom":false}],
 "grants":[{"socket":"/home/me/.cache/agent.sock","workspace":"agent"}]}
```

## Configuration

The optional config is `$XDG_CONFIG_HOME/fux/config.fux`, else
`~/.config/fux/config.fux`: fuxth, which each client runs as it attaches.
Its `set` words change the server for everyone; its key words change that
client's keys alone. `fux str FILE check` reads a file without running it
and says what is wrong.

```sh
# ~/.config/fux/config.fux
str C-a prefix
str shell str '/bin/zsh -l' set
str clipboard str off set

unbind-all                                        # optional: start from no bindings
define split-right me focused horizontal split drop end   # a word of the config's
str v str split-right bind
str 't n' str 'opts new-tab drop' bind            # t is a layer: C-b t n
str 'r l' str 'me focused str right 1 move-border' bind  repeat   # C-b r l l l, then Esc
str g str 'me focused vertical str lazygit program split drop' bind
  str 'lazygit' label  str Tools group            # what the column shows, and under which heading
str M-h str 'me str left focus' bind-root         # no prefix: Alt-h alone

# C-b y: the focused pane's history in your editor, in a split; `env`
# gives the program the pane's id, and `"${FUX:-fux}"` is the server's fux,
# on PATH or not; after a failure, Enter closes it
define edit-history
  me focused vertical
  str FUX_TARGET me focused env
  str sh program str -c arg
  str 'f=$(mktemp) && "${FUX:-fux}" "$FUX_TARGET" all-history capture >"$f" && ${EDITOR:-vi} "$f" || read -r _; rm -f "$f"; "${FUX:-fux}" here kill' arg
  split drop
end
str y str edit-history bind  str 'edit history' label  str Panes group
```

| Setting | Value | Default |
| --- | --- | --- |
| `shell` | a program and its arguments | `$SHELL`, else your login shell, else `/bin/sh` |
| `history-bytes` | bytes of each pane's history, how far copy mode scrolls and `history capture` reads; past it, the oldest rows go; 0 to 1 GiB; a change applies to every pane | 8388608 (8 MiB) |
| `wait-seconds` | how long a wait waits at most unless it says (`SECS timeout`), 0 to 86,400 | 30 |
| `clipboard` | `on` or `off`: copies, fux's and programs', reach your terminal's clipboard | `on` |
| `buffers` | paste buffers kept, 1 to 1000 | 16 |
| `bell` | `on` or `off`: a pane's bell rings your terminal and marks its tab | `on` |
| `titles` | `on` or `off`: your terminal's title follows the focused pane | `off` |
| `update-environment` | variable names: a new pane takes these from whoever starts it, rather than from the server, which may have started long ago; one they lack is unset, but for `FUX`, `FUX_PANE`, `FUX_SOCKET` and `FUX_SERVER` | `DISPLAY KRB5CCNAME SSH_AGENT_PID SSH_ASKPASS SSH_AUTH_SOCK SSH_CONNECTION WAYLAND_DISPLAY XAUTHORITY` |

**Bindings** run fuxth, compiled when bound, run fresh each time their keys
are typed, as your client (`me`). A binding, and the command prompt, may use
the config's definitions, written anywhere in it; not its `let` names (a
definition that reads one is refused there, with that reason). `bind` takes the keys after the prefix,
space-separated (`str 't n'` makes `t` a layer); `bind-root` one key, with
no prefix, which then never reaches any program: bind chords or function
keys rather than letters. A key sequence is a binding or a layer, never
both. Esc after the prefix always closes the column. `label`, `group` and
`repeat` change the binding just made; the column lists groups in the order
their keys were first bound, `Other` last (a key bound again keeps its place).

The default keys are a config like yours, run before it, so anything one
binding does, yours may: rebind a key, `unbind` it, or start from none.
`fux list-keys` prints your keys as config lines that make them, from
`unbind-all` on; not your definitions, which a binding may call, so add
them to your config rather than replace it: `fux list-keys >>
~/.config/fux/config.fux` gives you every key to edit in place. What it
appends starts with `unbind-all`, which undoes any key lines earlier in your
config, so delete those. Default keys a later fux adds then reach you only
once you bind them.

## Saved sessions

`dump` prints what there is as a fuxth program that makes it again, each
pane a `let` name later lines use; `X dump-of` one tab or workspace. Run it
to make it again on a running server, each tab in the workspace of its
name (made if missing; a tab's name is the server's alone, so close what it
holds first): `fux str dev workspace dump-of > dev.fux; fux dev.fux`.

A **server saves its session** the same way, with the ids it had, about a
second after each change, in `$XDG_STATE_HOME/fux` (`~/.local/state/fux`),
written whole and moved in place, yours alone. A server started on the same
socket after a crash, a `kill -9` or a reboot runs it to bring the session
back with the same ids: shells start fresh in their directories; a program
`run` started waits, saying so on its screen, until `respawn` starts it
(`paused` in `ls json`). `kill-server` and the last pane closing forget it.

## Terminal features

- **Mouse.** fux turns on your terminal's mouse reporting only while the
  focused pane's program asks for it (htop, `vim` with `mouse=a`, lazygit)
  and nothing of fux's is open, and gives that program its clicks, drags
  and wheel in its own cells and encoding. Clicks elsewhere (another pane,
  a border, the bar) do nothing; a drag that leaves the pane stays at its
  edge. The rest of the time your terminal selects text and scrolls as it
  always does; while a program has the mouse, most terminals select with
  Shift held. fux itself never acts on the mouse: no clicking to focus,
  dragging borders or scrolling history (copy mode does that).
- **Keyboard.** If your terminal speaks the kitty keyboard protocol
  (Ghostty, kitty, WezTerm, foot, iTerm2), fux turns it on while attached,
  telling apart keys otherwise sent alike (Shift-Enter and Enter, Ctrl-I
  and Tab); Escape needs no wait. Each program gets keys as it asked: kitty
  protocol, xterm's modifyOtherKeys, or plain. The decoder and encoder are
  fux-vt's (`fux_vt::keys`), so other hosts of fux-vt can use them.
- **Synchronized output** (mode 2026): frames reach your terminal whole; a
  frame is shown anyway after one second or past 2 MiB.
- **Colours.** Programs asking the foreground and background (OSC 10, 11)
  or palette entries 0 to 15 (OSC 4) are told your terminal's, and get its
  dark/light changes (mode 2031) if it reports them. With several terminals
  attached, a pane is answered from the one that typed into its tab last.
  Colours a program sets (OSC 4, 10, 11) apply to its own pane only, and
  win over your terminal's; your terminal's palette is never changed.
- **Bells.** A program's bell rings each terminal showing its workspace, at
  most once every 250 ms; a tab you are not looking at is marked `!` in the
  bar until you show it. `set bell off` silences both.
- **Titles.** With `set titles on`, your terminal's title is the focused
  pane's (or its tab's name). fux saves your terminal's own title first and
  restores it when you detach or turn titles off.
- **Hyperlinks** (OSC 8) reach your terminal as links.
- **Pictures** (sixel). A pane's program is told your terminal draws sixel
  graphics, and the size of its cells (`CSI 16 t`), if your terminal says
  so, and the pictures it draws are shown on your terminal, clipped to the
  pane; they scroll with the text, and text over one, or an overlay, takes
  it away. A pane no sixel-drawing terminal shows is told none, so its
  programs draw in text (half blocks), which captures read; `json` says
  where pictures are. Drawn by foot, WezTerm, xterm (`-ti vt340`), Konsole,
  mlterm and others; not by kitty, whose graphics protocol fux does not
  speak, nor Ghostty, Alacritty or Apple's Terminal.
- **Underline styles** (curly, dotted, dashed, double) reach your terminal
  if it draws them, else a plain underline.
- **In-band resize reports** (mode 2048) are sent when a pane's size
  changes.

## Not included

- **fux's own mouse actions**: the mouse reaches only programs that ask for
  it (see [Terminal features](#terminal-features)).
- **Hooks** (tmux's `set-hook`): read `fux events` and run fux
  instead.
- **Programs that outlive their server**: a crash ends them; the next server
  brings back the layout, and `PANE respawn` starts each again.
- **A format language** for output (tmux's `#{…}`): `json`, and `X str
  FIELD get` for one value.
- **Watching the config file**: a client runs it as it attaches.

## Security

The server listens only on a Unix socket: `str PATH socket`, `str NAME
server` (`NAME.sock` in fux's directory), else `FUX_SOCKET`, else
`$XDG_RUNTIME_DIR/fux/server.sock`, else `$TMPDIR/fux/server.sock`. An
auto-started server logs to `fux.log` beside it.

- The socket's directory must be yours, mode 0700, reached only through
  directories no other user can change; fux creates the default one so.
- The socket is mode 0600 before any connection is accepted, and a lock
  file makes one server its owner.
- Every peer must run as the server's own user (`SO_PEERCRED` on Linux,
  `getpeereid` on macOS); others, root included, are refused.
- Messages are length-prefixed and at most 1 MiB.

Anyone who can open the socket can run anything as you (`run`,
`send-keys`). As with tmux, the socket's permissions are the access
control. The server never interprets a program: it takes typed requests,
and from attached terminals typed actions and literal keys for a pane.

Against mistakes, rather than against a program of yours that means harm:

- `kill-server` from one of the server's own panes, or while a person's
  terminal is attached (headless clients do not count), is refused:
  `force-kill-server` stops it anyway. `kill` takes a handle, never a
  default, so an empty variable closes nothing.
- `fux str WS str PATH grant` makes a second socket whose requests reach
  workspace WS alone: an agent given only it can run, read and close what is
  there, where you can watch it in your own server, and nothing else (no
  other workspace, the settings, the buffers, clients or the server). `fux
  str PATH revoke` closes it. Keys it sends into a shell you started there
  run as you, with your shell's own socket: grant a workspace of the agent's
  own panes.
- `fux watch` watches without typing into anything.

## Development

CI runs, on Linux and macOS:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The tests start real servers and shells: they need `/bin/sh`, `/bin/dash`
and `python3`, and use zsh if installed. Tools outside the workspace, each
with a README:

- [`fux-vt/compare`](https://github.com/gold-silver-copper/fux/blob/master/fux-vt/compare/README.md):
  fux's terminal emulator beside other terminals;
- [`diff`](https://github.com/gold-silver-copper/fux/blob/master/diff/README.md):
  fux beside its last release;
- [`bench`](https://github.com/gold-silver-copper/fux/blob/master/bench/README.md):
  speed, latency and footprint;
- [`fuzz`](https://github.com/gold-silver-copper/fux/blob/master/fuzz/README.md):
  libFuzzer targets;
- [`walk`](https://github.com/gold-silver-copper/fux/blob/master/walk/README.md):
  scripted runs of the real binary.

fuxth itself is its own crate, [`fuxth`](fuxth/README.md): the language with
no words of fux's, for any program that wants a small Forth to drive it.

## License

MIT
