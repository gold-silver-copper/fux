# fux

A small terminal multiplexer. One server keeps your shells running in
workspaces, tabs and split panes; `fux` attaches a terminal to it, and every
other `fux` command changes it. Several terminals can attach at once, each
with its own view. fux is driven from the keyboard: it has no mouse actions
of its own, by design, and passes the mouse only to a program that asks for
it (see [Terminal features](#terminal-features)).

fux is one binary for macOS and Linux, built with Rust 1.95 or later.

```sh
cargo install --locked fux        # or, in a clone: cargo install --locked --path .
fux                               # attach, starting a server if none runs
```

## Using it

`fux` (or `fux attach`) attaches this terminal, starting a server in the
background if none answers; `-t NAME` picks a workspace. A new server has
one workspace, tab and shell. `C-b d` detaches, and the shells keep
running. Inside a fux pane, `fux` refuses to attach unless given
`--nested`.

The server runs until `fux kill-server`, SIGTERM, SIGINT, SIGHUP, or until
its last pane closes. It then hangs up every pane, tells each attached
terminal why, and removes its socket.

Every pane runs your shell (`set shell`, else `$SHELL`, else `/bin/sh`). A
command after `--` in `split`, `new-tab` or `new-workspace` is typed into
the new shell at its first prompt, so it gets your aliases and history,
and the shell remains when it ends. A pane closes when its shell exits,
showing its viewers the exit status; a tab closes with its last pane, a
workspace with its last tab. A pane's lines re-wrap when its width changes,
history included.

The bottom row is the bar: the workspace and its tabs on the left; on the
right the focused pane's number and title (the program's OSC 0/2 title,
else the pane's name), a notice, or copy mode's position.

### Keys

Every key below follows the prefix, `C-b` by default, and is a lower-case
letter: case counts, so `C-b T` is not `C-b t` (with Caps Lock on, fux
says `C-b T` is not bound). The prefix twice sends it to the pane
(`send-prefix`).

The prefix alone opens the **command column**, the help screen, listing
every binding, and those without the prefix last: arrows, PageUp/PageDown
and Home/End move, Enter runs the selected command, a bound key runs its
own, Esc closes. A key you bind after the prefix is yours there, even one
the column moves with (`bind Up …`); Esc always closes. Commands that
cannot run now are dimmed.

| Key | Command | Does |
| --- | --- | --- |
| `h` `j` `k` `l` | `select-pane -L` / `-D` / `-U` / `-R` | focus the pane that way |
| `o` / `q` | `select-pane --next` / `--last` | focus the next / last pane |
| `v` / `s` | `split -h` / `split -v` | split side by side / stacked |
| `x` | `confirm-close pane` | close the pane (asks `y`/`n`) |
| `z` | `zoom` | zoom the pane, or restore it |
| `a` | `menu pane` | pane actions |
| `c` | `copy-mode` | copy and select |
| `p` | `paste-buffer` | paste the newest copy |
| `n` / `b` | `select-tab --next` / `--previous` | next / previous tab |
| `e` | `command-prompt` | type any fux command |
| `d` | `detach` | detach this terminal |
| `r`, then `h` `j` `k` `l` | `resize-pane -L` / `-D` / `-U` / `-R` | move a border one cell |
| `m`, then `h` `j` `k` `l` | `move-pane -L` / `-D` / `-U` / `-R` | move the pane beside its neighbour that way |
| `t`, `w` | tab and workspace layers | below |

`r` and `m` are **repeat modes**: `h` `j` `k` `l` act again without the
prefix until Esc or Enter; the prefix ends the mode and opens the command
column; any other key ends it and is dropped, which the bar says.

`t` and `w` are **layers**, sharing their verbs:

| Verb | After `t`: tabs | After `w`: workspaces |
| --- | --- | --- |
| `n` | `new-tab` | `new-workspace` |
| `h` / `l` | `select-tab --previous` / `--next` | `select-workspace --previous` / `--next` |
| `g` | `choose-tab` | `choose-workspace` |
| `r` | `rename-prompt tab` | `rename-prompt workspace` |
| `x` | `confirm-close tab` | `confirm-close workspace` |
| `a` | `menu tab` | `menu workspace` |
| `m`, then `h` / `l` | `reorder tab --previous` / `--next`, repeating | `reorder workspace --previous` / `--next`, repeating |

`f`, `g`, `i`, `u` and `y` are free for your own bindings.

- **Choosers** (`t g`, `w g`) list tabs or workspaces with their panes.
  Enter selects, `r` renames, `x` closes, Esc or `q` cancels; arrows,
  `j`/`k`, PageUp/PageDown and Home/End move.
- **Action menus** (`a`, `t a`, `w a`) hold what has no key: rename, close,
  terminate the running command, swap, move elsewhere, reorder. A menu acts
  on the item it was opened for, even once renamed, and closes if that item
  is gone.
- **The command prompt** (`e`) runs a fux command line, such as
  `split -v -- htop`, and shows its output in the bar. It and the rename
  prompts edit with the arrows, Home, End, Backspace and Delete.

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
`OSC 133 ; A`, which fish sends, and zsh and bash send with a terminal's
shell-integration script. fux does not pass these marks to your terminal,
so its own jump-to-prompt does not work inside fux.

A copy keeps wide characters whole, joins soft-wrapped lines, and trims
trailing blanks. One copy is at most 262,144 cells.

**Clipboards.** A copy goes to fux's paste buffers (the newest 16, `set
buffers`); `C-b p` pastes the newest, bracketed if the program asked for
bracketed paste. It is also sent to your terminal's clipboard as OSC 52,
up to 1 MiB encoded, unless `set clipboard off`; your terminal must allow
OSC 52 writes. A program in a pane that copies with OSC 52, as Neovim and
Helix do, copies the same way: to the buffers, and to the clipboard of
each terminal showing the pane. fux never reads the clipboard.

## Commands

Commands run from the command line (`fux COMMAND …`), a key binding or the
command prompt; `fux help` lists them, and `fux help COMMAND` (or
`fux COMMAND --help`) explains one, its flags and an example. Targets: a pane is `%N`, a tab
`@N`, a workspace `+N` or its name, a client `cN`. Inside a pane,
`FUX_PANE` and `FUX_SOCKET` name the pane and the server, so commands there
need no `-t`. Elsewhere a command that needs a target and has none fails.
From a key or the prompt, commands act on your focused pane.

| Command | Does |
| --- | --- |
| `fux` / `fux attach [-t WS] [--nested]` | attach, starting a server if none answers |
| `fux attach --headless [--size COLSxROWS] [--log FILE]` | a client with no terminal, for tests and scripts: prints its id, records what it is sent to FILE, ends on a signal |
| `fux server [--socket PATH] [--config FILE]` | run a server in the foreground |
| `fux kill-server` | stop the server |
| `fux ls [--json]` | workspaces, tabs, panes and clients |
| `fux new-workspace [-n NAME] [--size COLSxROWS] [--json] [-- CMD…]` | a workspace with a shell, CMD typed into it; prints `+N @N %N`. Names are unique, and do not start with `+`, `%`, `@` or `-`, which targets read otherwise |
| `fux new-tab [-t WS] [-n NAME] [--size COLSxROWS] [--json] [-- CMD…]` | a tab with a shell, CMD typed into it; prints `@N %N` |
| `fux split -h\|-v [-t %N] [--cwd DIR] [--env K=V] [--json] [-- CMD…]` | split a pane: `-h` side by side, `-v` stacked; prints `%N`. One CMD word is typed as a line (`'cd src && make'`); several are quoted, as a program's arguments |
| `fux run [-t WS] [-n NAME] [--size COLSxROWS] [--cwd DIR] [--env K=V] [--log FILE] [--wait-… [--timeout SECS] [--print]] [--json] -- PROGRAM…` | start a program in place of a shell, in a tab of its own in workspace `agent` (or WS), no client moved to it; prints `%N`. Its pane stays once it exits, with its last screen and status. `--log` keeps all it writes in FILE; `-t NAME` then finds the pane by its tab's name, and a name already in use is refused. `--wait-text T`, `--wait-idle MS`, `--wait-exit` and `--wait-bell` wait as `wait` does from its first output on, printing the pane id and then what the wait found |
| `fux wait [-t %N]… --text T\|--gone T\|--idle MS\|--change [--since N]\|--exit\|--done\|--bell [--timeout SECS] [--print] [--marks] [--json]` | wait until the pane shows text, even in a line that scrolled away, and print that line (`--text` again for any of several); stops showing it; shows no change for MS (redrawing the same screen is no change); changes (past revision N); its program exits (prints `exited STATUS`); a shell command ends (OSC 133, prints `done STATUS`); or it rings the bell or notifies. `-t` again watches several panes. Status 3 if the program exits first (its last lines printed), 124 after `--timeout` (60 s), saying what the pane is doing. `--print` prints the screen as it ends, `--json` what happened |
| `fux respawn [-t %N] [-k] [--env K=V] [--wait-…] [-- PROGRAM…]` | start again in its pane what `run` started, its id and size kept; `-k` ends it first if it still runs; waits as `run` does |
| `fux notify [-t TARGET] TEXT` | a notice on the clients showing the target, or every client |
| `fux resize -t %N\|@N\|+N COLSxROWS\|auto` | what a pane's tab, a tab (or a workspace's tabs) is laid out on while no client shows it; `auto`: a client's screen |
| `fux resize -c CLIENT COLSxROWS` | a headless client's size |
| `fux kill-pane\|kill-tab\|kill-workspace [-t …]` | close, without asking; from the command line, prints `closed` and the ids gone. A tab or its pane may be named by the tab's name |
| `fux signal [-t %N] TERM\|KILL\|INT\|HUP\|QUIT\|USR1\|USR2\|STOP\|CONT [--wait-…]` | a signal to what runs in the pane's foreground (what its keys reach: a shell's job, or the program `run` started), never a shell itself; for a program that ignores Ctrl-C and hangups; waits as `send-keys` does |
| `fux rename -t TARGET NAME` | rename a pane, tab or workspace |
| `fux move-pane [-t %N] --to @N\|+N\|new-tab\|new-workspace` | move a pane (to a workspace: its first tab) |
| `fux move-pane [-t %N] -L\|-R\|-U\|-D` | move a pane beside its neighbour that way |
| `fux swap-pane [-t %N] %M\|-L\|-R\|-U\|-D` | swap two panes, anywhere |
| `fux resize-pane [-t %N] -L\|-R\|-U\|-D [CELLS]` | move the nearest border (default one cell) |
| `fux resize-pane [-t %N] [-x COLS[%]] [-y ROWS[%]]` | make a pane a width, a height or both, in cells or a percentage of its tab; refused, nothing changed, if it cannot be |
| `fux reorder pane\|tab\|workspace [-t TARGET] --next\|--previous` | move one place in its order |
| `fux send-keys [-t %N \| -c CLIENT] [-l] [-N COUNT] KEYS… [--wait-text T\|--wait-idle MS\|… [--timeout SECS] [--print] [--marks] [--json]]` | send keys (`C-c`, `Enter`, …); other words, all with `-l`, and `--text TEXT` as text, but a mistyped key name (`Entr`) is refused; `--paste TEXT` as a paste, bracketed if the program asked. `-N` sends them COUNT times. `--wait-text`, `--wait-gone`, `--wait-idle`, `--wait-change`, `--wait-exit`, `--wait-done` and `--wait-bell` then wait as `wait` does, armed before the keys go in; `--wait-idle` exits 5 if they changed nothing. A line holding the text typed is its echo, and does not count for `--wait-text`. `-c` types them at a client instead, as at its terminal: its bindings, overlays and prompts take them |
| `fux send-mouse [-t %N] click\|right\|middle\|press\|release\|wheel-up\|wheel-down\|move ROW COL [--wait-…]` | a mouse event at a cell, from 0, encoded as the program asked; refused if it did not ask for mouse reports. Waits as `send-keys` does: `--wait-idle` exits 5 if the click changed nothing |
| `fux send-prefix [-t %N]` | send the prefix key to the pane, as its program asked for keys |
| `fux capture-pane [-t %N] [-S -LINES\|all] [--full] [-J] [--rows A-B] [--cols A-B] [--styles] [--marks] [--main] [--alt] [--last-output] [--json]` | the pane's text, after LINES of history (or all), trailing blanks and blank rows trimmed (`--full` keeps them); `-J` joins soft-wrapped rows; `--rows`/`--cols` a region, from 0; `--styles` colours and attributes as SGR; `--marks` reverse video, how a selection is drawn, as `⟦beta⟧`; `--main` the main screen under a full-screen program; `--alt` the full screen it drew last, after it left or exited; `--last-output` the last shell command's output (OSC 133) |
| `fux capture-client [-c CLIENT] [--json]` | what a client's terminal shows, bar included |
| `fux set`, `bind`, `unbind`, `unbind-all` | change the configuration |
| `fux reload` | run the config file again over the defaults |
| `fux list-buffers`, `show-buffer [-b N]`, `paste-buffer [-b N] [-t %N]` | paste buffers, newest `0` |
| `fux list-keys` | key names and the current bindings |
| `fux detach [-c CLIENT]` | detach a client |
| `fux help [COMMAND\|agents]`, `fux --version` | usage, one command at length, the recipe for scripts and agents; version |

These act on a client's screen: yours from a key or the prompt, `-c CLIENT`
from the command line. `zoom` and `copy-mode` take no `-t`.

- `command-column`, `command-prompt`, `copy-mode`, `zoom`
- `choose-tab [-t %N] [--move]`, `choose-workspace [-t %N] [--move]` (with
  a pane: choose where to move it), `choose-pane [-t %N]` (to swap with)
- `menu pane|tab|workspace [-t TARGET]`,
  `rename-prompt [pane|tab|workspace] [-t TARGET]`,
  `confirm-close [pane|tab|workspace] [-t TARGET]`
- `select-pane -t %N|--next|--previous|--last|-L|-R|-U|-D`,
  `select-tab -t @N|--next|--previous`,
  `select-workspace -t WS|--next|--previous`

Exit status: 0 done; 1 failed, with the reason on stderr; 2 a usage error,
with the command's usage. A wait also exits 3 when the program exited
first, 4 when the pane closed, 5 when `--wait-idle` saw no change and 124
on a timeout; one met by the program's exit exits with its status, as
`timeout(1)` does. A tmux name (`list-panes`, `new-window`, …) gets
a hint at fux's.

**Scripts and agents** drive programs with `run`, `send-keys`, `wait` and
`capture-pane`, each step one call, without touching anyone's tabs:

```sh
P=$(fux run --size 100x30 -- htop)         # out of every client's way
fux wait -t $P --text Load                 # until it has drawn
fux send-keys -t $P F6 --wait-idle 200 --print   # press, settle, read
fux send-keys -t $P q && fux wait -t $P --exit   # prints: exited 0
fux kill-workspace -t agent                # everything run started
```

`fux help agents` prints the whole recipe, the same as
[docs/agents.md](docs/agents.md).

**Key names**, for `send-keys` and `set prefix`, are tmux's (`C-x`, `M-x`,
`S-Left`, `Enter`, `BTab`, `BSpace`, `PageUp`, `F1`–`F12`, …) and any
single character, which carries its own shift (`T`, not `S-t`).
`fux list-keys` prints them all.

**Inside a pane**, `TERM` is `xterm-256color`, `TERM_PROGRAM` is `fux` and
`TERM_PROGRAM_VERSION` is fux's version. XTVERSION (`CSI > q`) answers
`fux` and its version, secondary device attributes (`CSI > c`) the
version, and primary ones (`CSI c`) a VT220-class terminal.

### `--json`

These shapes are part of fux's interface; changing one is a breaking
change.

```json
{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main",
  "panes":[{"id":"%1","name":"zsh","title":"","rows":23,"cols":80,"pid":4242,
    "program":"vim","exited":null,"signal":null,"idle_ms":1520,"alt_screen":true,
    "mouse":true,"bracketed_paste":true,"pictures":[],"cursor_visible":true,
    "history_lines":0,"copy_mode":false,"revision":38,"running":false,
    "last_status":null,"bells":0,"notification":null,"cwd":"/home/me",
    "argv":["vim","notes.md"]}]}]}],
 "clients":[{"id":"c1","rows":24,"cols":80,"workspace":"+1","tab":"@1","pane":"%1","zoom":false}]}
```

That is `fux ls --json`. Each pane says what it runs in its foreground
(`program`, `null` if unknown), the status its program exited with
(`exited`, for a pane `run` started, else `null`) or the signal that
killed it (`signal`, its number: 11 for SIGSEGV), how long its screen has shown no change
(`idle_ms`), and whether its program is on the alternate screen, asked
for mouse reports and for bracketed paste. `revision` grows with every
change to the screen (`fux wait --change --since N`); `running` and
`last_status` come from the shell's OSC 133 marks; `bells` counts bells
and notifications, and `notification` is the last one's text. `cwd` and
`argv` are the foreground process's, where the system says (Linux).

`fux capture-pane --json` prints
`{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":[…]}` and the
same pane fields: the history asked for with `-S` then the screen, trimmed
as without `--json`. With `--styles` it adds `"styles":[[RUN…]…]`, a list
per line of its runs of cells that are not plain, each
`{"col":0,"len":3,"fg":1,"bg":"#1e1e20","bold":true,"inverse":true}` with
the colours (palette numbers, or `#rrggbb`) and attributes it has.
`fux capture-client --json` prints
`{"client":"c1","rows":24,"cols":80,"cursor":[ROW,COL],"lines":[…]}`,
every row the client shows, with `cursor` `null` while it is hidden.
Rows and columns count from 0.

`split`, `new-tab`, `new-workspace` and `run` with `--json` print where
the new pane is: `{"workspace":"+2","tab":"@3","pane":"%4"}`.

## Configuration

The optional config file is `--config`, else
`$XDG_CONFIG_HOME/fux/fux.conf`, else `~/.config/fux/fux.conf`. It holds
`set`, `bind`, `unbind` and `unbind-all` lines, split into words like a
shell (quotes, backslashes, `#` comments).

```sh
# ~/.config/fux/fux.conf
set prefix C-a
set shell /bin/zsh -l
set clipboard off

unbind-all                       # optional: start from no bindings
bind v split -h
bind t n new-tab                 # t is a layer: C-b t n
bind -r r l resize-pane -R       # -r repeats: C-b r l l l, then Esc
bind -g Tools g split -v -- lazygit   # -g: its group in the command column
bind V split -v                  # case counts: C-b V, Shift-v
bind Left select-pane -L         # any key after the prefix: C-b Left
bind -n M-h select-pane -L       # -n: no prefix, Alt-h alone
```

| Option | Value | Default |
| --- | --- | --- |
| `prefix` | a key name; not one bound with `bind -n` | `C-b` |
| `shell` | a program and its arguments | `$SHELL`, else `/bin/sh` |
| `history-lines` | lines per pane, 0 to 1,000,000; a change applies to every pane | 10000 |
| `clipboard` | `on` or `off`: copies, fux's and programs', reach your terminal's clipboard | `on` |
| `buffers` | paste buffers kept, 1 to 1000 | 16 |
| `bell` | `on` or `off`: a pane's bell rings your terminal and marks its tab | `on` |
| `titles` | `on` or `off`: your terminal's title follows the focused pane | `off` |
| `shell-marks` | `on` or `off`: the bash, zsh and fish fux starts mark each command (OSC 133) after reading your own startup files, so `wait --done` and `capture-pane --last-output` work with no setup | `on` |

**Bindings** take the keys after the prefix, as separate words before the
command: any key `fux list-keys` names (`Up`, `F5`, `Space`, `Enter`, …),
any character (`1`, `:`), and chords (`M-h`, `C-Left`). Case counts: `V`
is Shift-v, and `S-v` is read as `V`. A letter with Ctrl has no case
(`C-V` is `C-v`). `bind t n new-tab` makes `t` a layer. A key sequence is
a command or a layer, never both: `bind t zoom` is refused while `t` is a
layer, and `unbind t` removes the layer. Esc after the prefix always
closes the column, so it cannot be bound. The command is checked when the
binding is made.

**Without the prefix**, `bind -n KEY COMMAND` binds one key that acts at
once, and `unbind -n KEY` removes it. There are none by default. Any key
but the prefix can be bound, and a key bound so never reaches any program,
in any pane: bind chords (`M-h`) or function keys rather than letters. It
acts only while you type into a pane: in copy mode, the command column, a
menu or a prompt, the key is theirs. `unbind-all` removes these too.

`set`, `bind` and `unbind` are ordinary commands, so they change a running
server from the command line or the prompt too. `fux reload` reruns the
file over the defaults, and says so when there is no file; on an error it names the file and line and changes
nothing. A server started with an invalid file runs on the defaults and
shows the error to each terminal that attaches until a reload succeeds.

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
  programs draw in text (half blocks), which captures read; `--json` says
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
- **Saving and loading layouts**: a script of `fux split` and `fux new-tab
  -- CMD` lines makes one.
- **Hooks** (tmux's `set-hook`): watch `fux ls --json` and run commands
  instead.
- **Surviving a server restart**: as in tmux, panes end with their server.
- **Watching the config file**: run `fux reload`.

## Security

The server listens only on a Unix socket: `fux server --socket PATH`, else
`FUX_SOCKET`, else `$XDG_RUNTIME_DIR/fux/server.sock`, else
`$TMPDIR/fux/server.sock`. An auto-started server logs to `fux.log` beside
it.

- The socket's directory must be yours, mode 0700, reached only through
  directories no other user can change; fux creates the default one so.
- The socket is mode 0600 before any connection is accepted, and a lock
  file makes one server its owner.
- Every peer must run as the server's own user (`SO_PEERCRED` on Linux,
  `getpeereid` on macOS); others, root included, are refused.
- Messages are length-prefixed and at most 1 MiB.

Anyone who can open the socket can run anything as you (`split -- CMD`,
`send-keys`). As with tmux, the socket's permissions are the access
control.

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

## License

MIT
