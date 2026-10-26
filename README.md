# fux

A small terminal multiplexer. One server keeps your shells running in
workspaces, tabs and split panes; `fux` attaches a terminal to it. Every
other `fux` command line is a short program in **fuxth**, a tiny Forth made
for the shell, and so are fux's config, key bindings, command prompt and
dumped sessions. Several terminals can attach at once, each with its own
view and its own keys. fux is driven from the keyboard: it has no mouse
actions of its own, by design, and passes the mouse only to a program that
asks for it (see [Terminal features](#terminal-features)).

fux is one binary for macOS and Linux, built with Rust 1.95 or later.

```sh
cargo install --locked fux        # or, in a clone: cargo install --locked --path .
fux                               # attach, starting a server if none runs
```

## Using it

`fux` alone attaches this terminal (it runs `opts attach`), starting a
server in the background if none answers; `fux opts str work in attach`
attaches to workspace `work`, and `fux opts watch attach` only watches
(keys do nothing but `C-b d`, which detaches). A new server holds
nothing: the first client to attach starts workspace `main`, with one tab
and a shell, on its screen. One that watches starts nothing: with no
server, or one holding nothing, it is refused. `C-b d`
detaches, and the shells keep running. Inside a fux pane, `fux` refuses to attach.

The server runs until `fux kill-server` (refused while a person's terminal
is attached or from its own panes: [Security](#security)), SIGTERM,
SIGINT, SIGHUP, or until its last pane closes. It then hangs up every
pane, tells each attached terminal why, and removes its socket.

A pane runs your shell (the `shell` setting: `fux help set`), or the program
it was given (`run`, `split`). A shell's pane closes when its shell exits,
showing its viewers the exit status; a program's pane stays, its exit told,
until `kill` closes it or `respawn` starts it again. A tab closes with its
last pane, a workspace with its last tab.
A pane's lines re-wrap when its width changes, history included.

The bottom row is the bar: the workspace and its tabs on the left; on the
right the focused pane's number and title (the program's OSC 0/2 title,
else the pane's name), a notice, or copy mode's position. A screen too
small for its tab (a watcher's, or one too small for all its panes)
shows panes cut off at their right and bottom, or none of a pane it has
no room for: the bar names them before the title, hidden first: `[hidden %3] [cut %2]`.

## fuxth

A fux command line is a program: words run left to right, each taking its
values from a stack and leaving its own.

```sh
fux ls                                            # what there is
A=$(fux opts 80 24 size str htop program run)     # start htop: %3:0.0.0.0.0:RUN, its version
fux str "$A" changed wait                         # until it differs: %3:0.412.0.0.0:RUN screen
fux %3 capture                                    # its screen
fux %3 str q text send-keys                       # press q: its version from before
```

- A number is a value; `%3`, `@2` and `+1` alone are a pane, a tab and a
  workspace; any other bare word is a word to run. `str WORD` pushes WORD as
  text, whatever it looks like: `str 'two words'`.
- What is left at the end is printed, a value a line: a pane as `%3`, a tab
  `@2`, a workspace `+1`, a client `c1`, a capture or listing as its text.
  `json` turns a value into JSON: `fux ls json`, `fux str %3 pane info json`.
- Every pane has a **version**, one word that names the pane, counts what
  only grows while its program runs (respawns, its screen's revision, bells,
  shell commands ended, its end) and names the server's run. An action
  (`run`, `send-keys`, `signal` …) acts at once and answers its pane's
  version from just before it acted; `str V changed wait` waits in V's pane
  until its version differs from V, and answers what changed. fux never
  judges whether a program is done: [docs/agents.md](docs/agents.md) says
  what a version counts and has recipes (a text, quiet for N seconds, a
  command's end, an exit). A failure prints `fux: CODE: MESSAGE` on stderr,
  with the file, line and word, and exits 1; a wait exits 4 if the pane
  closed, 124 if nothing changed in time.
- A single argument is a whole program (`fux 'ls json'`), never a file,
  whatever it holds; `fux --file FILE ARGS…` runs a file, so a script can
  start with `#!/usr/bin/env -S fux --file`; and `fux -` runs what stdin
  says. `#` starts a comment.
- The language's core is small on purpose: a few words and parsing words
  (`str`, `define`, `let`, `if`, `begin`, `list`, `each`, `try`), listed by
  `fux help` and at length in [fuxth/README.md](fuxth/README.md).
  The rest of the language, its standard library, is written in fuxth;
  fux's own words (`ls`, `run`, `send-keys` …) are fux's, not the
  language's.
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
`fux help agents` is the recipe for scripts and agents, its first screen
enough for most work and `fux help TOPIC` the rest
([docs/agents.md](docs/agents.md)), `fux help json` what `json` gives
([docs/json.md](docs/json.md)), and `fux help limits` the bounds fux
keeps.

What a word that acts takes (**options**, a wait, keys, a mouse event) goes
on the pane before it, in any order: the pane given them becomes a request,
which the word at the end acts on. `run`, `new-tab` and `new-workspace` have
no pane yet, nor has `attach`: they start from `opts`, empty options. Of
the first three, only `run` takes a program (`program`, then each `arg`; a
shell's line is `str sh program str -c arg str LINE arg`); `new-tab` and
`new-workspace` start a shell. Their tab is laid out on `COLS ROWS size`, else on the
caller's own screen (a client's keys), else on the caller's tab (`fux`
run in a pane): never on the screen of a client that does not show the
caller's tab. Run from outside fux, a size must be given: fux makes none
up.

```sh
fux opts str web name 120 40 size str ./app program str --verbose arg run
fux %3 200 history capture
fux %3 str 'make test' text str Enter key send-keys 60 timeout wait
```

A program can name what it made; a wait on an action's answer waits from the version it holds:

```sh
fux opts 80 24 size str sh program str -c arg str 'echo hello; sleep 1' arg run let P   P wait drop   P capture
```

A script plays well with the shell. `args` gives the arguments after
`--file FILE`, `input` the rest of stdin, `read-file` a file's text (UTF-8,
16 MiB at most), `getenv` a variable (one not set fails `unset_var`, never
an empty string); `warn` prints on stderr, and `N exit` ends the program
with exit status N:

```sh
$ cat greet.fux
#!/usr/bin/env -S fux --file
args each str 'hello ' swap cat print end
$ ./greet.fux ann bob
hello ann
hello bob
```

These words are the command line's alone: a config's or a key's program is
refused them (`not_here`). `fux ./greet.fux` is refused as an unknown word,
the error saying `--file` runs it: no argument is guessed to be a file.

The bundled words are written in fuxth themselves (`fux help WORD` shows
each one's code): `type` and `press` (text and a key into a pane), `line`
(a line typed and Enter), `shell` (a tab with a shell in workspace
`agent`), `quiet` (a wait until nothing changes for N seconds) and
`counts` (a version's counts, as integers).

### Finding things

A pane is `%N`, a tab `@N`, a workspace `+N` or its name, a client `cN`:
`str %3 pane`, `str @2 tab`, `str work workspace`, `str c1 client`. `pane`
also takes a tab's name, the tab's one pane (`run`'s tabs are named after
their program), `tab` and `workspace` a pane, the tab or workspace it is
in. A pane's own name (what `rename` gave it, else its program's) finds it
through `pane-named` alone: `str logs pane-named`. A title a program sets
finds nothing. Inside a pane,
`here` is that pane (`FUX_PANE`, below). In a key's program or the
prompt's, `me` is the client whose key it is, and `me focused` its focused
pane.

### Which server

`str NAME server` before the rest is the server named NAME (its socket
`NAME.sock` in fux's directory, `$XDG_RUNTIME_DIR/fux` or, with
`XDG_RUNTIME_DIR` unset, `$TMPDIR/fux`), `str PATH socket` a socket's path;
without either, `FUX_SOCKET`, else the default. A private server for a test
or an agent:

```sh
fux str test server start                         # returns once it answers: nothing in it yet
fux str test server opts 80 24 size str ./app program run
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
Enter runs the selected one, a bound key runs its own, Esc closes; its moves
by keys that are not letters are the choosers' (`str list … bind-in`). A key you
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
| `p` | `me focused 0 paste-buffer drop` | paste the newest copy |
| `n` / `b` | `me str next step-tab` / `me str previous step-tab` | next / previous tab |
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
- **Prompts and questions of your own:** `prompt` opens a line whose Enter
  runs your fuxth with the line on its stack, a string, and `confirm` a
  question whose `y` runs it (`me str 'close it?' str '%3 kill' confirm`).
  The rename prompts and close questions are library words made of them.
- **These screens' keys are a config too**, as the bindings are: `bind-in`
  gives a key of copy mode, a chooser, a line being typed or a question
  one of that screen's actions, and `label` names it in the screen's hints
  (`str copy str g str top bind-in str top label`); `unbind-in` takes one
  away. keys.fux binds every action; a name that is none is refused with
  the list. Escape always leaves a screen, and is never bound.
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

Its keys, as keys.fux binds them: letters, in either case, and the
brackets; the arrows, PageUp/PageDown, Home, End, Enter and Esc also work.

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

**Clipboards.** A copy goes to fux's paste buffers (as many of the newest
as `buffers` says); `C-b p` pastes the newest, bracketed if the program asked for
bracketed paste. It is also sent to your terminal's clipboard as OSC 52, up
to 1 MiB encoded, unless `str clipboard false set`; your terminal must
allow OSC 52 writes. A program in a pane that copies with OSC 52, as Neovim
and Helix do, copies the same way, up to 64 KiB encoded, but only while
`clipboard` is true: set false, a program's copy copies nothing, to the
buffers either. A longer copy, or one that is not base64 (RFC 4648,
padded or not) of UTF-8 text, copies nothing, and the clients showing
the pane say so. fux never reads the clipboard.

## Shell marks

fux starts your shell as it is, and reads the OSC 133 marks any program
sends: `C` where a command's output begins, `D;STATUS` where it ends, `A`
where a prompt starts. With them, a pane's version counts the commands
that end, `last_status` says how the last did, `last-output` reads its
output alone, and copy mode's `[` and `]` jump between prompts; without
them, each says there are none. `fux help shells` shows the lines
that send them, for bash, zsh and fish, to put in your own startup files
([docs/agents.md](docs/agents.md#shells-shells-and-their-marks)).

## Words

`fux help` lists every word, by what it does, with what each takes and
leaves; `fux help WORD` says one at length.

**Key names**, for `key`, `bind` and `prefix`, are tmux's (`C-x`, `M-x`,
`S-Left`, `Enter`, `BTab`, `BSpace`, `PageUp`, `F1`–`F35`, …) and any
single character, which carries its own shift (`T`, not `S-t`); common
spellings (`Backspace`, `Esc`, `Return`, `Del`, `Ins`, `PgUp`, `PgDn`) are
taken too, and `key` tells a name it refuses the closest one, if any.
`fux key-names` prints them all.

**Inside a pane**, a program's environment holds:

| Variable | Value |
| --- | --- |
| `TERM` | `xterm-256color` |
| `TERM_PROGRAM`, `TERM_PROGRAM_VERSION` | `fux` and fux's version, not the outer terminal's, whose features fux may not have |
| `FUX_PANE` | the pane, `%N`: `here` to a fux command run there |
| `FUX_SOCKET` | the socket of the pane's server, so a fux command there reaches it |
| `FUX_SERVER` | which run of which server the pane is of, the run every version names. A fux command from a pane of another server, or of an earlier run of this one, or with `FUX_PANE` and no `FUX_SERVER`, has no `here` there (`stray_pane`); `kill-server` from one of the server's own panes is refused (`own_server`) |
| `FUX` | the fux executable the server runs, unset if the server cannot name it: what a key's program or a script in a pane runs, as `"${FUX:-fux}"`, when `fux` may not be on its PATH (a server started from a desktop launcher or a systemd unit) |

The rest is the server's own environment, as it was when the server
started. Two things override it and the table alike: the names in
`update-environment`, which a pane takes from whoever starts it (`fux help
set` says what one the starter lacks becomes), and, last, what a program's
`env` sets (`opts str NAME str VALUE env 80 24 size str PROGRAM program
run`). A starter with no environment at all, as `env -i` leaves one,
changes none.

XTVERSION (`CSI > q`) answers `fux` and its version, secondary device
attributes (`CSI > c`) the version, and primary ones (`CSI c`) a
VT220-class terminal.

### JSON

`json` on what a word left gives its JSON: `ls json`, `X info json`,
`pane capture json`, `client capture-client json`, an action's or a
wait's answer (`… wait json`), `status json`; `events` and `X events-of`
stream an object a line. These shapes are part of fux's interface; `fux
help json` lists every one ([docs/json.md](docs/json.md)).

## Configuration

The optional config is `$XDG_CONFIG_HOME/fux/config.fux`, else
`~/.config/fux/config.fux`: fuxth, which each client runs as it attaches.
Its `set` words change the server for everyone, all at once when it has
run; its key words change that client's keys alone. Half a config is no
config: one that fails sets nothing, the client keeps the default keys,
and it says why. `fux str FILE check` reads a file without running it and
says what is wrong.

```sh
# ~/.config/fux/config.fux
str C-a prefix
str shell list str /bin/zsh str -l end set       # a program, then its arguments, a string each
str clipboard false set

unbind-all                                        # optional: no keys at all, the screens' too
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

`fux help set` lists the settings: what each takes, its default and what
it does.

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
binding does, yours may: rebind a key, `unbind` it, or start from none
(`unbind-all`, which takes the screens' keys too; Escape still leaves them).
`fux list-keys` prints your keys as config lines that make them, from
`unbind-all` on; not your definitions, which a binding may call, so add
them to your config rather than replace it: `fux list-keys >>
~/.config/fux/config.fux` gives you every key to edit in place. What it
appends starts with `unbind-all`, which undoes any key lines earlier in your
config, so delete those. Default keys a later fux adds then reach you only
once you bind them.

## Saved sessions

`dump` prints what there is as a fuxth script that makes it again (its first
line `#!/usr/bin/env -S fux --file`, so `chmod +x` makes it runnable), each
pane a `let` name later lines use; `X dump-of` one tab or workspace. Run it
to make it again on a running server, each tab in the workspace of its
name (made if missing; a tab's name is the server's alone, so close what it
holds first): `fux str dev workspace dump-of > dev.fux; fux --file dev.fux`. Each
tab is made at the size it has (`size`), which a server with no client
needs; one that fits clients' screens fits them again after
(`resize-auto`). Each split is made with shares (`2 1 shares`: the pane
split keeps two thirds of its room, the new one takes a third), so the
layout comes back as it was, at every size. A split
or a move that would hide a pane, on its tab's size or on the screen of a
client showing it (but one that sizes nothing: that only watches, or has
no room below its bar), is refused: whatever there
is can be made again. Each split holds its two shares exactly, never
rounded. A close gives the closed pane's room to its sibling in their
split, every other share as it was. Moving a border changes only the panes
that touch it, and once those beyond are at their least the next border
out moves on: each split it moves takes the cells its two sides then have
as its shares. A terminal's resize changes no share.

Nothing is saved on its own. To keep a session past its server (a crash, a
`kill -9`, a reboot), dump it when you choose, and replay the dump on a
fresh server:

```sh
fux dump > ~/session.fux              # save when you choose
fux start && fux --file ~/session.fux # replay on a fresh server
```

A dump makes the layouts and launches again: each shell starts afresh in
its directory, each program `run` started starts again. It never brings
back what a process held: its screen, its history, what ran in a shell. A
pane whose directory is not UTF-8 text, which fuxth cannot write, or a
shell's directory that fux cannot read, is dumped without it, and a comment line says
so: it starts where the dump is run.

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
  frame is shown anyway after 1 s or past 2 MiB.
- **One terminal answers for a tab.** With several terminals attached,
  a tab's panes are answered (colours, scheme, sixel, cell size) by one
  terminal, the tab's host: of those showing the tab, the one that last
  typed into it, else the first attached; none in a tab no terminal shows
  (a headless client has no terminal: it never hosts, and its keys don't
  change who does). What the host didn't say is unknown, never taken from
  another terminal or kept from an earlier host: a host that draws no
  sixel means no pictures, one that said no background leaves it
  unanswered, and one that said no cell size leaves `CSI 16 t`
  unanswered.
- **Colours.** Programs asking the foreground and background (OSC 10, 11)
  or palette entries 0 to 15 (OSC 4) are told the host's, and get its
  dark/light changes (mode 2031) if it reports them: one whose tab had
  another host, or none, is told the scheme of the host it has once it
  has one. Colours a program sets (OSC 4, 10, 11) apply to its own pane
  only, and win over your terminal's; your terminal's palette is never
  changed. A colour no one knows is not answered: one the host didn't
  say, palette entries past 15 the program didn't set, and every colour
  in a tab with no host.
- **Bells.** A program's bell rings each terminal showing its workspace, at
  most once every 250 ms; a tab you are not looking at is marked `!` in the
  bar until you show it. `str bell false set` silences both.
- **Titles.** With `str titles true set`, your terminal's title is the
  focused pane's (or its tab's name). fux saves your terminal's own title
  first and restores it when you detach or turn titles off.
- **Hyperlinks** (OSC 8) reach your terminal as links.
- **Pictures** (sixel). A pane's program is told your terminal draws sixel
  graphics, and the size of its cells (`CSI 16 t`, asked again each time
  your terminal is resized, as a font zoom resizes it), if your terminal
  says so (its terminal's size in pixels, `TIOCGWINSZ`, follows the cell,
  and a change signals it, SIGWINCH), and the pictures it draws are shown on
  your terminal, clipped to the pane; they scroll with the text, and text
  over one, or an overlay, takes it away. A pane is told its tab's host's
  sixel and cell size, at once as they change (its terminal is sized in
  pixels for the cell, which signals its programs): with no host, or a host
  that said no cell size, it is told it has none. A pane whose host
  doesn't draw sixel, or said no cell size, is told no sixel, so its
  programs draw in text (half blocks), which captures read; it keeps no new
  picture, and shows those it has on no terminal until its host draws
  sixel again, though another terminal showing its tab does. A picture
  shows on each terminal showing it that draws sixel in the cell size it
  was laid out in. `json` says where pictures are, and the cell size
  (`cell`, `null` while it is told none). fux never makes a cell size
  up: a terminal says none with a 0, or with one smaller than a sixel band, 2
  by 6 pixels, which fux tells you. A pane told none leaves `CSI 14 t` and
  `CSI 16 t` unanswered; told one, it answers them with it. A sixel
  picture is placed only while the pane's host draws sixel and said its
  cell size.
  Drawn by foot, WezTerm, xterm (`-ti vt340`), Konsole, mlterm and others;
  not by kitty, whose graphics protocol fux does not speak, nor Ghostty,
  Alacritty or Apple's Terminal.
- **Underline styles** (curly, dotted, dashed, double) reach your terminal
  if it draws them, else a plain underline.
- **In-band resize reports** (mode 2048) are sent when a pane's size
  changes.

## Not included

- **Pane logs** (tmux's `pipe-pane`): run the program under `script` in its
  pane, so it still writes to a terminal, rather than to a pipe as with
  `tee`. On Linux (util-linux):

  ```sh
  fux opts str script program str -q arg str -e arg str -f arg str LOG arg str -c arg str 'make test' arg run
  ```

  On macOS and the BSDs, whose `script` takes the command as its last
  words and flushes with `-F`:

  ```sh
  fux opts str script program str -q arg str -F arg str LOG arg str sh arg str -c arg str 'make test' arg run
  ```

  LOG holds the raw bytes the program wrote to its terminal; util-linux
  adds a first and a last line of its own, which BSD `-q` leaves out. `-e`
  (util-linux) makes the pane's exit status the program's, which BSD
  `script` always gives.
- **fux's own mouse actions**: the mouse reaches only programs that ask for
  it (see [Terminal features](#terminal-features)).
- **Mouse reports in pixels** (mode 1016): your terminal tells fux the
  cell the mouse is in, not the pixel within it, and fux makes none up.
  The mode is reported permanently reset and setting it changes nothing,
  so a program reports in cells, as with a terminal without it.
- **Hooks** (tmux's `set-hook`): read `fux events` and run fux
  instead.
- **Programs that outlive their server**, or a session saved on its own: a
  crash ends them; replay a `dump` you saved to make the layout again.
- **A format language** for output (tmux's `#{…}`): `json`, and `X str
  FIELD get` for one value.
- **Watching the config file**: a client runs it as it attaches.

## Security

The server listens only on a Unix socket, which is the only way to
control it: a sandbox that denies `connect` to it cannot drive fux. The
socket is `str PATH socket`, `str NAME
server` (`NAME.sock` in fux's directory), else `FUX_SOCKET`, else
`server.sock` there. fux's directory is `$XDG_RUNTIME_DIR/fux` whenever
`XDG_RUNTIME_DIR` is set, else `$TMPDIR/fux`; one of them set empty or to
a relative path is refused, as is neither set. An auto-started server logs
to `fux.log` beside it.

- The socket's directory must be a directory of yours, not a symbolic
  link, mode 0700; fux creates the default one so. The directories above
  it are not checked.
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
- To keep an agent to panes of its own, give it a server of its own, a
  session apart from yours:

  ```sh
  FUX_SOCKET=/run/user/1000/agent.sock fux start
  FUX_SOCKET=/run/user/1000/agent.sock fux opts 120 40 size new-workspace
  ```

  An agent given only that socket reaches only what runs there; you watch
  it by attaching to the socket (`FUX_SOCKET=… fux opts watch attach`, or `fux`
  to type there too). Panes never move between servers. It keeps a mistake
  in one session from reaching the other, nothing more: both servers run as
  you, so neither is kept from your own processes and files.
- `fux opts watch attach` watches without typing into anything.

## Development

CI runs, on Linux and macOS, these and the rest of
[its gates](.github/workflows/ci.yml):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The tests start real servers and shells: they need `/bin/sh`, `/bin/bash` and
`python3`, use `dash` if it is on `PATH`, and with `FUX_TEST_SHELLS=1`
also zsh and fish. Tools outside the workspace, each
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
