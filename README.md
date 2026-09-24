# fux

A small terminal multiplexer. One server keeps your shells running in
workspaces, tabs and split panes; `fux` attaches a terminal to it, and every
other `fux` command changes it. Several terminals can attach at once, each
with its own view. Everything is driven from the keyboard: there is no mouse
support at all, by design.

fux is one binary with no runtime dependencies beyond the system: it needs a
Unix (macOS or Linux), and builds with Rust 1.95 or later.

```sh
cargo install --locked --path .       # or: cargo build --release --locked
fux                                   # attach, starting a server if none runs
```

## Using it

`fux` (or `fux attach`) attaches this terminal to the server, starting one in
the background if none answers. A new server starts with one workspace, one
tab and one shell. `C-b d` detaches; the shells keep running, and `fux`
attaches again. `fux attach -t NAME` attaches to a workspace by name.
Running `fux` inside a fux pane is refused, since it would show fux inside
itself; `--nested` does it anyway.

The server keeps running until `fux kill-server`, SIGTERM, SIGINT or SIGHUP,
or until its last pane closes. Then it hangs up every pane, tells each
attached terminal why, and removes its socket.

Every pane runs your shell (`set shell`, else `$SHELL`, else `/bin/sh`). A
command given to `split`, `new-tab` or `new-workspace` after `--` is typed
into that shell, as if you had typed it: it runs with your aliases and
shell setup, lands in the shell's history, and when it ends, the prompt is
back in the same pane. A pane closes when its shell exits (`exit`, `C-d`),
and its viewers are told the exit status. A tab closes with its last pane,
and a workspace with its last tab; a tab emptied by moving its panes out
stays, showing how to split or close it.

The bottom row is the bar: the workspace and its tabs on the left (the
selected tab highlighted), and on the right the focused pane's number and
title (a program's OSC 0/2 title, else the pane's name), a notice, or copy
mode's position.

### Keys

Every key below follows the prefix, `C-b` by default. The prefix alone opens
the **command column**, which lists every binding, grouped: Up/Down or `j`/`k`,
PageUp/PageDown and Home/End move through it, Enter runs the selected
command, a bound key runs its command directly, and Esc closes it. Commands
that cannot run now are dimmed, and running one says why. The prefix twice
sends it to the pane. This column is the only help screen.

| Key | Command | Does |
| --- | --- | --- |
| `h` / `v` | `split -h` / `split -v` | split side by side / stacked |
| `x` | `confirm-close pane` | close the pane (asks `y`/`n`) |
| `z` | `zoom` | zoom the focused pane, or restore |
| `r` | `rename-prompt pane` | rename the pane |
| `p` | `menu pane` | pane actions |
| `c` | `copy-mode` | copy and select |
| `P` | `paste-buffer` | paste the newest copy |
| `C-Left` … `C-Down` | `resize-pane -L` … `-D` | move a border by one cell |
| `S-Left` … `S-Down` | `move-pane -L` … `-D` | move the pane beside its neighbour that way |
| Tab / `BTab` / `BSpace` | `select-pane --next` / `--previous` / `--last` | focus the next, previous or last pane |
| `M-Left` … `M-Down` | `select-pane -L` … `-D` | focus the pane that way |
| `t` / `T` | `new-tab` / `choose-tab` | new tab / tab chooser |
| `]` / `[` | `select-tab --next` / `--previous` | next / previous tab |
| `s` | `menu tab` | tab actions |
| `w` / `W` | `new-workspace` / `choose-workspace` | new workspace / workspace chooser |
| `}` / `{` | `select-workspace --next` / `--previous` | next / previous workspace |
| `S` | `menu workspace` | workspace actions |
| `:` | `command-prompt` | type any fux command |
| `d` | `detach` | detach this terminal |

**Choosers** (`T`, `W`) list each tab or workspace with its panes, the current
one marked. Enter selects, `r` renames, `x` closes (after asking), Esc or `q`
cancels; Up/Down, `j`/`k`, PageUp/PageDown and Home/End move.

**Action menus** (`p`, `s`, `S`) hold what has no key of its own: rename,
close, terminate the running command, swap, move to another or a new tab or
workspace, reorder. A menu acts on the item it was opened for, even if focus
changes meanwhile; if that item is gone, the menu closes and says so.

**The command prompt** (`:`) takes any fux command, in the same grammar as the
command line and the config file, for example `:split -v -- htop`. Its output
or error shows in the bar.

Directional focus picks, among the panes beyond the focused pane's edge, the
one whose centre is closest across the direction, then along it, then the
lowest number.

### Copy and select

`C-b c` puts a keyboard cursor on the focused pane, starting at its text
cursor. For your terminal the pane holds still while you look, even as output
continues (other terminals see it live); the bar shows `COPY` and the
cursor's line in the history.

| Keys | Do |
| --- | --- |
| `h` `j` `k` `l`, arrows | move |
| `w` `b` `e`, `W` `B` `E` | by word, or by space-separated word |
| `0` `^` `$`, Home, End | start of line, first non-blank, last non-blank |
| `H` `M` `L` | top, middle, bottom of the view |
| `g` / `G` | top of the history / the live bottom |
| `C-u` / `C-d`, PageUp / PageDown (`C-b` / `C-f`) | half a page / a page |
| `/` `?` then `n` `N` | search forward or back, again, the other way |
| `v` / `V` / `C-v` | select characters / lines / a block; again to clear |
| `o` | swap the selection's ends |
| `y` or Enter | copy and leave |
| `q` or Esc | leave without copying |

Search is literal (not a regular expression) over the whole history, and
ignores case unless the query has a capital letter. A selection keeps wide
characters and combining marks whole, joins soft-wrapped lines without an
invented newline, and trims trailing blanks. One copy is at most 262,144
cells.

A copy goes into fux's paste buffers (the newest 16, `set buffers`), where
`C-b P` pastes the newest into the focused pane, bracketed if the pane asked
for bracketed paste. It is also sent to your terminal's clipboard as OSC 52,
up to 1 MiB encoded, unless `set clipboard off`; your terminal must allow
OSC 52 writes (many do; some ask first or need an option). fux never reads
the clipboard.

## Commands

Every command runs from the command line (`fux COMMAND …`), from a key
binding, from the `:` prompt, and, for `set`/`bind`/`unbind`, from the config
file. Targets: a pane is `%N`, a tab `@N`, a workspace `+N` or its name (`$N`
is avoided because the shell would expand it). A client is `cN`.

Inside a pane, `FUX_PANE` names it and `FUX_SOCKET` names the server, so
commands there target that pane without `-t`. A command that needs a target
and has neither `-t` nor `FUX_PANE` fails with a message naming `-t`; it never
guesses. From a key or the `:` prompt, commands act on your focused pane,
tab and workspace.

| Command | Does |
| --- | --- |
| `fux` / `fux attach [-t WS] [--nested]` | attach, starting a server if none answers |
| `fux server [--socket PATH] [--config FILE]` | run a server in the foreground |
| `fux kill-server` | stop the server (hangs up every pane) |
| `fux ls [--json]` | workspaces, tabs, panes and clients |
| `fux new-workspace [-n NAME] [-- CMD…]` | a workspace with a shell, CMD typed into it |
| `fux new-tab [-t WS] [-n NAME] [-- CMD…]` | a tab with a shell, CMD typed into it |
| `fux split -h\|-v [-t %N] [-- CMD…]` | split a pane: `-h` side by side, `-v` stacked |
| `fux kill-pane\|kill-tab\|kill-workspace [-t …]` | close, without asking |
| `fux rename -t TARGET NAME` | rename a pane, tab or workspace |
| `fux move-pane [-t %N] --to @N\|+N\|new-tab\|new-workspace` | move a pane (to a workspace: its first tab) |
| `fux move-pane [-t %N] -L\|-R\|-U\|-D` | move a pane beside its neighbour that way |
| `fux swap-pane [-t %N] %M` or `-L\|-R\|-U\|-D` | swap two panes, anywhere |
| `fux resize-pane [-t %N] -L\|-R\|-U\|-D [CELLS]` | move the nearest border that way (default one cell) |
| `fux reorder pane\|tab\|workspace [-t TARGET] --next\|--previous` | move one place in its order |
| `fux terminate [-t %N]` | SIGTERM to what runs in the pane's foreground, not the shell |
| `fux send-keys [-t %N] [-l] KEYS…` | send keys (`C-c`, `Enter`, …), or literal text with `-l` |
| `fux capture-pane [-t %N] [-S -LINES] [--json]` | the pane's screen text, with LINES of history before it |
| `fux set OPTION VALUE`, `fux bind [-g GROUP] KEY CMD…`, `fux unbind KEY`, `fux unbind-all` | change the running configuration |
| `fux reload` | run the config file again over the defaults |
| `fux list-buffers`, `fux show-buffer [-b N]`, `fux paste-buffer [-b N] [-t %N]` | paste buffers, newest `0` |
| `fux list-keys` | key names, and the current bindings |
| `fux detach [-c CLIENT]` | detach a client |

These act on one client's screen. From a key or the `:` prompt they act on
yours; from the command line they need `-c CLIENT`:
`command-column`, `command-prompt`, `copy-mode`, `zoom`,
`choose-tab [--move]`, `choose-workspace [--move]`, `choose-pane [-t %N]`
(swap), `menu pane|tab|workspace [-t TARGET]`,
`rename-prompt [pane|tab|workspace] [-t TARGET]`,
`confirm-close [pane|tab|workspace] [-t TARGET]`,
`select-pane -t %N|--next|--previous|--last|-L|-R|-U|-D`,
`select-tab -t @N|--next|--previous`, `select-workspace -t WS|--next|--previous`.

Exit status: 0 done; 1 the command failed, with the reason on stderr; 2 a
usage error.

Key names, for `bind`, `send-keys` and the prefix, are tmux's: `C-x`, `M-x`,
`S-Left`, `Enter`, `Tab`, `BTab`, `Escape`, `Space`, `BSpace`, `Up`, `Down`,
`Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, `Delete`,
`F1`–`F12` (also `PgUp`, `PgDn`, `NPage`, `PPage`, `IC`, `DC`), plus any single
character. A character carries its own shift (`T`, not `S-t`). `fux list-keys`
prints them. The Kitty keyboard protocol is not supported: keys use xterm
encodings.

### `--json`

`fux ls --json` prints one object:

```json
{"workspaces":[{"id":"+1","name":"main","tabs":[{"id":"@1","name":"main",
  "panes":[{"id":"%1","name":"zsh","title":"","rows":23,"cols":80,"pid":4242}]}]}],
 "clients":[{"id":"c1","rows":24,"cols":80,"workspace":"+1","tab":"@1","pane":"%1","zoom":false}]}
```

`fux capture-pane --json` prints
`{"pane":"%1","rows":23,"cols":80,"cursor":[ROW,COL],"lines":[…]}`, where
`lines` is the history asked for with `-S` followed by the screen, each line's
trailing blanks trimmed. These shapes are part of fux's interface: changing
one is a breaking change.

## Configuration

The config file is a list of fux commands, one per line, as in `tmux.conf`:
`--config`, else `$XDG_CONFIG_HOME/fux/fux.conf`, else
`~/.config/fux/fux.conf`. It is optional.

```sh
# ~/.config/fux/fux.conf
set prefix C-a
set shell /bin/zsh -l            # default: $SHELL, else /bin/sh
set history-lines 10000          # per pane
set clipboard off                # default: on (OSC 52 writes)
set buffers 16                   # paste buffers kept

unbind-all                       # optional: start from no bindings
bind h split -h
bind v split -v
bind d detach
bind -g Tools g split -v -- lazygit   # -g puts it under a column group
```

A line is split into words like a shell: whitespace separates, `'…'` is
literal, `"…"` allows backslash escapes, a backslash escapes outside quotes,
and `#` starts a comment. A binding's command is the rest of its line. `set`,
`bind` and `unbind` are ordinary commands, so `fux bind x kill-pane` or
`:set clipboard off` change a running server the same way.

`fux reload` runs the file again over the defaults. On any error it names the
file and line and keeps the previous configuration whole. At startup an
invalid file does not stop the server: it runs on the defaults, logs the
error, and shows it to each terminal that attaches until a reload succeeds.
Only `set`, `bind`, `unbind` and `unbind-all` may appear in the file.

Options: `prefix` (a key), `shell` (a program and its arguments),
`history-lines` (0 to 1,000,000), `clipboard` (`on` or `off`), `buffers` (1
to 1000).

## Security

The server listens only on a Unix domain socket: `FUX_SOCKET`, else
`$XDG_RUNTIME_DIR/fux/server.sock`, else `$TMPDIR/fux/server.sock`
(`fux server --socket PATH` overrides it). The socket's directory must be
yours with mode 0700, reached only through directories no other user can
change; the default `fux` directory is created that way, and nothing that
already exists is modified. The socket is created with mode 0600 before any
connection is accepted. A lock file beside it makes one server its only
owner; a socket left by a killed server is replaced only when nothing
answers on it; and at exit the server removes the socket only if it is still
the one it bound. Every connection's peer must run as the server's own user
(checked with `SO_PEERCRED` on Linux, `getpeereid` on macOS); others,
including root, are refused. An auto-started server logs to `fux.log` beside
the socket.

Anyone who can open the socket can run anything as you: `split -- CMD` and
`send-keys` exist for exactly that. The socket's permissions are the access
control, as with tmux. Every message is length-prefixed and at most 1 MiB,
and the whole interface is the fixed command list above.

## How it works

One server thread runs a `poll` loop over the socket, every client and every
pane's PTY; there are no other threads and no async runtime. Each pane has a
PTY and a [`fux-vt`](fux-vt) terminal emulator. Layout is a tree of weighted
splits per tab; each client gets its own rectangles for its own size, and a
PTY's size is the smallest rectangle any client shows it in. A client is a
dumb pipe: its keystrokes go to the server as raw bytes and are decoded
there, and the server paints each client's screen from a cell grid, sending
only what changed, at most once per 16 ms, inside synchronized output. A
client that stops reading gets nothing more queued until it catches up, then
one full repaint.

The previous, Bevy-based fux is kept at the tag `bevy-final`.

## License

MIT
