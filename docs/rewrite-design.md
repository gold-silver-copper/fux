# fux without Bevy: design

Status: agreed with the user on 2026-09-23, and being implemented on the
branch `rewrite`. This is the specification; `docs/prompt-rewrite-without-bevy.md`
is how to build it. Where the code had to depart from the first version of
this document, the change is made here and its commit says why.

**Scope of the first version (decided 2026-09-24): functionality only.** It
builds everything below that a user sees and uses: panes, tabs, workspaces,
independent views, the command column, choosers and menus, copy/select, the
CLI and the config file. Each part lands with the ordinary unit and
integration tests that show it works. Fuzzing, benchmarks, a black-box
harness, lesson-by-lesson regression tests and long CI jobs are left for a
later version; see "Later, not in the first version".

## Why

Bevy and its remote protocol (BRP) caused most of what hunts 5–8 found:

- The generic RPC surface: 003, 004, 005, 009 and 015 (despawning or mutating
  entities, including resource entities, and removing resources).
- Exposing it safely: 002, 006, 007, 012 and 014 (browser access, descriptor
  and body limits, socket cleanup).
- Bevy's runtime: 011 (ALSA in the build), 018 and 019 (a shared I/O pool),
  and an O(tabs) layout cost from projecting each viewer through `bevy_ui`.
- Friction: 17 pinned `=0.20.0-rc.1` crates, a 170 MB debug binary, slow
  builds.

A multiplexer needs none of that. It needs PTYs, a terminal emulator, a split
tree, a socket and a render loop.

## Decisions

| Question | Answer |
| --- | --- |
| Detach and reattach | Yes: a server outlives the terminal that started it. |
| Several clients on one server | Yes, each with an **independent view**, as today. |
| Save and load layouts | No. |
| Mouse | **None.** fux never enables mouse reporting; the outer terminal keeps its own mouse behaviour. |
| Selecting and copying | A keyboard **copy/select mode**: a movable cursor over the pane and its history. |
| Menus | The navigable command column, tab and workspace choosers, and pane/tab/workspace action menus, all keyboard-driven. |
| A pane's program | **Always the user's shell.** A command given with `-- CMD` is typed into that shell, as if the user had typed it; when it ends, the prompt is back. A pane closes only when its shell exits. |
| Configuration | A file of fux commands. Zero dependencies. |
| Clipboard | OSC 52 writes **on by default** (`set clipboard off` disables). Nothing ever reads the clipboard. |
| Control surface | The `fux` CLI only. No RPC, HTTP or JSON-RPC. |
| Bevy | None. No async runtime either. |
| System calls | `rustix`, not `nix`. `portable-pty` is replaced by rustix's PTY API. |

## What stays, what goes

**Stays, unchanged:** `fux-vt`, published as 0.1.1. It is the emulator, with
no dependency but `unicode-width`, and koh depends on it. The rewrite uses its
public API as it is. A fux-vt change is allowed only as a bug fix with a
failing test, must keep every item koh uses (`Parser`, `Options`, `Event`,
`Sink`, `Screen`, `Cell`, `Color`, the mouse enums, `Error`, and their
meanings), and is checked against koh in a throwaway clone. The branch
`feat/fux-vt-for-koh` also stays: koh pins fux-vt to it by git, and koh is not
touched.

**Ported (logic and tests, not files):**

| From | What | Bevy/nix today |
| --- | --- | --- |
| `src/encode.rs` | Key encoding for panes (application cursor, modifiers); its mouse half is dropped | none |
| `src/paste.rs` | Bracketed-paste decoding, bounded to 64 KiB | trivial |
| `src/transport.rs` | Socket path rules, 0700 directory checks, lock, stale-socket replacement, inode pin (014) | nix → rustix |
| `src/terminal.rs` | Session hangup on close (013), stopped ≠ exited (020), byte-bounded input (021), bounded drain, EINTR retry (010) | nix/portable-pty → rustix |
| `src/selection.rs` | Selection over fux-vt row IDs, surviving output and scrolling | light |
| `src/chrome.rs`, `src/interaction.rs`, `src/actions.rs` | The bottom bar, the command column, choosers, action menus, prompts; the action list with labels, groups and availability | light to moderate |

**Deleted:** `src/`, `tests/`, `fux-fuzz/` (a BRP-driven harness), and
`fux-agent-exercises/` (BRP-driven agent campaigns). Also deleted:
`verification/` and the BRP-specific docs. They stay retrievable at the tag
`bevy-final` (`e2b114f`, `main` after PR #52). `fux-fuzz/BREAKS.md` and the
repro scripts there are the record of mistakes not to repeat; the behaviour
each still-relevant finding led to is written into this design (the numbers
in parentheses), so building to the design avoids them.

## Architecture

```
fux (one binary, over a library crate so integration tests can speak the protocol)
 ├─ fux server         one thread, one poll loop
 ├─ fux attach         a thin client: raw bytes up, paint bytes down
 └─ fux <command>      one connection, one command, text (or --json) back
```

### The server

A single thread runs `rustix::event::poll` over:

- the listening socket (accept until `EAGAIN`, so one tick drains the whole
  backlog: 012);
- every client socket (reads and writes are nonblocking, with bounded buffers);
- every pane's PTY master (read up to 64 KiB a tick per pane, for fairness;
  write when its input queue is not empty);
- a self-pipe from `signal-hook`, for SIGCHLD, SIGTERM, SIGINT and SIGHUP.

The poll timeout is the nearest deadline among three: paint coalescing
(16 ms), a pending lone Escape (35 ms), and the kill grace after a hangup.

There are no threads, locks, channels or task pools, and nothing blocks the
loop. The only blocking call is `fork`/`exec`, which is quick. The config file
is read once, bounded, at startup and on `fux reload`.

**Lifetime.**
- `fux` or `fux attach` with no server answering starts one: it runs itself as
  `fux server`, in a new session with stdio on `/dev/null`, logging to
  `fux.log` beside the socket. It then connects, retrying for up to 2 s. Two
  racing starts are settled by the lock: the loser exits and both clients
  connect to the winner.
- The server runs until `fux kill-server`, SIGTERM, SIGINT or SIGHUP, or until
  it has no panes left. Then it hangs up every pane, tells each client why,
  removes its socket and exits.
- A new server starts with one workspace, holding one tab with one shell.
  Tabs and workspaces can be left empty by moving their panes out; an empty
  tab shows a hint to split or close it. A pane that closes (its shell
  exits, or it is killed) takes its tab with it when it was the tab's last
  pane, and a workspace goes with its last tab.

### Panes and processes

A pane owns:

- a PTY: `openpt(RDWR | NOCTTY)`, `grantpt`, `unlockpt`, `ptsname`,
  `tcsetwinsize`. rustix offers `OpenptFlags::CLOEXEC` only on Linux and the
  BSDs, so the master is set close-on-exec with `fcntl_setfd` straight after,
  on every platform. The slave is opened `O_CLOEXEC` too. The server has one
  thread, so no fork can happen in between. Every descriptor the server holds
  is close-on-exec, and a test checks that a pane's program inherits only
  stdio;
- a `fux_vt::Parser`;
- a child process: the configured shell (`set shell`), always, started
  through `std::process::Command` with the slave as stdio, `TERM=xterm-256color`, `FUX_PANE=%N` and `FUX_SOCKET`. A `pre_exec`
  hook calls `setsid` and `ioctl_tiocsctty`, both async-signal-safe, and
  resets the signal mask. std already restores SIGPIPE, and `exec` resets the
  handlers signal-hook installed. A test checks that a pane's program starts
  with default dispositions and an empty mask.

Each pane also has an ID, `%N`, which only increases, and a name.

**A command for a new pane** (`split`, `new-tab`, `new-workspace` with
`-- CMD…`) is typed into its shell, not run in place of it:

- fux puts the command line and a carriage return into the pane's input
  queue, as keystrokes, when the shell first writes output (normally its
  prompt), or after one second if it writes nothing. It is not wrapped in
  bracketed paste. Typing straight after the spawn was the first version of
  this: the terminal then echoed the line before the shell had drawn its
  prompt, so dash printed the command's output on the prompt's line (CI's
  Linux runner caught it in the milestone 5 tests), and bash and zsh showed
  the line twice.
- So the command runs in the user's interactive shell (aliases, functions,
  the PATH from its rc files), lands in its history (Up runs it again), and
  when it ends, or is interrupted with Ctrl-C, the prompt is back in the same
  pane.
- The line is built from the argv: each argument is kept bare if it has only
  `A–Z a–z 0–9 _ @ % + = : , . / -`, and otherwise put in single quotes, with
  each `'` written as `'\''`. That is literal in sh, dash, bash and zsh.
  fish (recognised by the shell's file name) also treats `\` inside single
  quotes as an escape, so for fish each `\` inside them is doubled.
  Arguments are joined with spaces.
- An argument with a control character (newline, tab, escape, …) is refused
  with an error naming it: a shell's line editor would act on it as a key
  (tab completes, newline runs a partial line). So is a line larger than the
  input queue.
- The command gets no exit status of its own from fux: `split -- CMD` returns
  once the pane exists, and the result shows in the pane like any command's.
- A remaining risk: a shell setup that writes output early in its startup
  and then discards pending input would still lose the command.

Process lifecycle, from the lessons:

- **Exit:** on SIGCHLD, and on EOF or EIO from the master, every live pane is
  probed with `waitid(Pid, EXITED | NOHANG | NOWAIT)`. Signals coalesce, so
  every pane is probed, not only one. Only an exited, killed or dumped status
  counts as an exit. macOS also reports stops here (020), so a stop is
  ignored.

  Once exited, the group is cleaned up (below), the leader reaped, and the
  pane closes; its clients see a notice with the exit status. The pane's
  program is its shell, so this happens when the user leaves the shell
  (`exit`, Ctrl-D) or something kills it, never merely because a command
  run in it ended.
- **Close:** SIGHUP to the leader's group and to every process in its session.
  The session is found through `/proc` on Linux and `proc_listallpids` on macOS
  (013), so a job started by dash survives only if it ignores the hangup, as
  under a real terminal. Then a 100 ms timer on the loop, never a sleep, closes
  the master, SIGKILLs the group and reaps the leader. Only IDs of unreaped
  children are ever signalled: the leader stays unreaped until then, so its
  PID and PGID cannot be reused.
- **Input:** a per-pane queue bounded by bytes (each piece costs its length
  plus 64; 16 × 64 KiB in total). It is never bounded by piece count (021).
- **Output:** read into the parser, with terminal replies (DSR, DA) appended
  to the same bounded input queue. When the queue is full, the program is not
  reading, the reply is dropped, and the pane records it once as a notice.

### Layout

State that everyone shares, owned by the server:

```
Server
 └─ Workspace $N (name)          ordered
     └─ Tab @N (name)            ordered
         └─ Node = Split { axis, children: [(weight, Node)] } | Pane %N
```

A plain tree of IDs is enough; nothing needs an ECS.

- Rectangles are computed per client from its size. Splits divide by weight,
  with one separator cell between siblings and a 2×2 minimum per pane.
- Directional focus uses rectangle centres, ranked by cross-axis distance,
  then forward distance, then ID, as today.
- An empty split collapses upward.
- A pane lives in exactly one tab. Moving a pane moves its process, and
  closing it ends the process.
- A command from any client or from the CLI can remove what another client is
  looking at. Each view then repairs itself deterministically:
  - focus goes to the last-focused surviving pane, else the first in tree
    order;
  - a closed tab selects its neighbour, a closed workspace the first
    remaining;
  - overlays and copy mode that referred to something removed close with a
    notice, and never act on a different item.

### Independent views

Each attached client has its own view:

- its size, and its current workspace;
- per workspace, the selected tab; per tab, the focused and last-focused pane;
- zoom;
- in copy/select mode, a scroll position for the pane it is on, anchored to a
  fux-vt row ID so that new output does not move what a scrolled-back client
  is reading. Leaving copy mode returns to the live screen, so this is the
  only scroll position a view keeps;
- its mode: normal, command column, chooser, action menu, prompt, confirm, or
  copy/select (with its cursor and selection);
- its last sent frame, used for diffing.

A PTY's size is the smallest rectangle it has among the clients currently
showing it, with a 1×1 minimum. A pane nobody shows keeps its last size. A
client whose rectangle is larger than the PTY sees the pane at the top left,
with the rest blank. Every change is published at once, and `fux ls` never
reports a size from before the last change. A client's size is clamped to
1..=4096 in each dimension.

### Input

The client is a dumb pipe:

- It puts the outer terminal in raw mode, on the alternate screen, with normal
  (not application) cursor and keypad modes, so that each key has one
  encoding.
- It enables bracketed paste and focus events, and never mouse reporting.
- It forwards raw bytes in frames of at most 64 KiB.
- On every exit path it restores the terminal: detach, server gone, a signal,
  or a panic, through a panic hook. That means leaving the alternate screen,
  showing the cursor, turning off the modes it set, and restoring termios.
  SIGWINCH sends `Resize`; SIGTERM, SIGHUP and SIGINT detach.
- Running `fux attach` inside a fux pane (`FUX_PANE` set) is refused, because
  it would nest the view inside itself. `--nested` overrides that.

The server decodes the bytes, per client:

1. **Decoding:** keys (CSI/SS3, UTF-8, a lone Escape after 35 ms) and
   bracketed-paste envelopes, bounded as before. Mouse sequences, which a
   correctly configured outer terminal never sends, are dropped.
2. **Routing by mode:**
   - The command column, choosers, menus, prompts, confirmations and
     copy/select mode own their input.
   - The prefix key twice sends it literally.
   - Pastes never become commands.
   - Otherwise the input goes to the focused pane, re-encoded for that pane's
     modes by the ported `encode.rs`.
   - Focus-in and focus-out go to that client's focused pane, only if the pane
     enabled focus reporting (`?1004`). fux-vt does not track that mode, nor
     the cursor shape (DECSCUSR), so fux scans each pane's output for those
     two itself, leaving fux-vt unchanged.
   - The Kitty keyboard protocol is not supported. Keys use xterm encodings.

**No mouse, deliberately.** Programs in panes that ask for the mouse (`vim`
with `mouse=a`, `htop`) get nothing, because the outer terminal is never put
in a mouse mode. Its wheel and text selection keep working natively, over
what is on screen. Selecting across history, or within one pane of a split,
is what copy/select mode is for.

Decoding on the server means one decoder and no key protocol to version, and
input can be tested without a PTY.

### Rendering

Per client, on a 16 ms coalescing tick:

1. Compose a cell grid from `fux_vt::Screen::window` for each visible pane,
   plus separators and the bottom bar (workspace, tabs, focused pane, notice).
   Overlays are drawn on top.
2. Diff it against the client's last grid, and emit only the changed runs,
   wrapped in synchronized output (`?2026`). Cursor position and shape come
   from the focused pane.
3. If a client's socket stops draining, stop queueing paints for it. Each
   client's output buffer is capped at 4 MiB. Once it drains again, send one
   full repaint. A slow client never grows the server and never stalls the
   loop.

Pane titles (OSC 0/2) are shown in the bar, not passed to the outer terminal.
Bells are not passed on either.

### Menus and overlays

All of these are keyboard-driven, drawn by the same renderer above the bar,
and private to the client that opened them:

- **The command column:** opened by the prefix key. It lists every binding,
  grouped (Panes, Focus, Tabs, Workspaces, Session, Other).
  - Up/Down or `j/k`, PageUp/PageDown and Home/End navigate; Enter runs the
    selected command; Esc cancels.
  - Pressing a bound key runs that binding directly.
  - Unavailable commands are dimmed, and running one explains why.
  - The list scrolls with `▲ n more` / `▼ n more` markers.
  - This is the only help surface.
- **Choosers:** a tab chooser and a workspace chooser, listing each item with
  its panes and the current one marked. Enter selects; `r` renames; `x` closes
  (with confirmation).
- **Action menus** for the focused pane, the current tab and the current
  workspace:
  - they hold what has no default key (terminate, reorder, swap, move to an
    existing or new tab or workspace, rename, close);
  - a menu acts on the item it was opened for, even if focus changes;
  - a disappeared item is never retargeted.
- **The command prompt** (`:`): type any fux command, with the same grammar as
  the CLI and the config file. Its output or error shows as a notice.
- **Prompts and confirmations:** a one-line editor (rename) and y/n
  confirmations for interactive closes.

### Copy/select mode

A keyboard cursor over the focused pane. For this client the pane holds
still while the mode is on: its rows are anchored by row ID, as scrolling is.
Output keeps arriving, and other clients see it live. The mode ends with a
notice if the pane closes or its history drops the anchored rows.

- **Enter it:** prefix `c`. The cursor starts at the pane's text cursor. The
  bar shows `COPY` with the cursor's line in history.
- **Move:**
  - `h j k l` or the arrows;
  - `w b e` and `W B E` by word;
  - `0 ^ $` within a line;
  - `H M L` within the view;
  - `g` and `G` to the top of history and to the live bottom;
  - Ctrl-U/D half a page, PageUp/PageDown a page.

  The view scrolls when the cursor leaves it.
- **Search:** `/` and `?`, then `n` and `N`, over the whole history. It is
  literal (not a regex) and smart-case: case-insensitive unless the query has
  a capital.
- **Select:** `v` characters, `V` lines, Ctrl-V a block (rectangle); `o` swaps
  the selection's ends; `v` again clears it.
- **Copy:** `y` or Enter copies and leaves the mode. `q` or Esc leaves without
  copying.
- **Where copies go:**
  - into fux's paste buffers, the last 16 by default, on the server; prefix
    `P` pastes the newest into the focused pane (bracketed if the pane asked
    for it);
  - `fux list-buffers`, `fux show-buffer` and `fux paste-buffer -t %N` work
    from scripts;
  - and to the copying client's own terminal as OSC 52, unless
    `set clipboard off`. The outer terminal must allow OSC 52 writes; many do
    by default, and some ask first or need an option.
- **Selection rules:**
  - rows are tracked by fux-vt row ID, as today, so new output and scrolling
    do not break a selection over rows that did not change;
  - wide glyphs and combining marks are kept whole;
  - soft-wrapped rows join without an invented newline, and trailing blanks
    are trimmed;
  - a copy is capped at 262,144 cells, and the clipboard at 1 MiB encoded;
  - nothing ever reads the clipboard.

### Security

A socket with the same rules as today:

- `FUX_SOCKET`, else `$XDG_RUNTIME_DIR/fux/server.sock`, else
  `$TMPDIR/fux/server.sock`.
- An owned 0700 directory, a 0600 socket, and a lock file.
- A stale socket is replaced only if nothing answers.
- The inode is pinned for cleanup (014).

Also:

- The peer's UID is checked on accept, and anyone but the server's own user
  is refused: rustix's `socket_peercred` on Linux, `getpeereid` through
  `libc` on macOS.
- The same user can run anything through fux (`split -- CMD`, `send-keys`).
  That is the design: the socket's permissions are the access control, as in
  tmux, and the README says so.
- Every message is length-prefixed with a hard size cap (007/008). Reads
  retry on `EINTR` (010).
- The whole surface is the fixed command list below. There is nothing like
  "despawn any entity".

## Protocol

It is private between one `fux` binary and itself. Frames are
`u32 length | u8 kind | payload`, with a 1 MiB cap on a frame; longer
payloads (a large paint, `capture-pane` of the whole history) are split
across frames. The first frame is `Hello` with `PROTOCOL`, an integer bumped
on any change to the protocol, and the crate version. A `PROTOCOL` mismatch
is refused with "server speaks protocol N (fux X); restart it with
`fux kill-server`". The `kill` role is accepted whatever the version, so
that advice always works.

| Direction | Frame |
| --- | --- |
| client → server | `Hello { protocol, version, role: attach\|command\|kill }` |
| client → server | `Attach { rows, cols, workspace? }` |
| client → server | `Input(bytes)`, `Resize { rows, cols }`, `Detach` |
| client → server | `Command { argv, cwd, pane? }` |
| server → client | `Paint(bytes)`, `Exit(reason)` |
| server → client | `Stdout(bytes)`, `Stderr(bytes)` (repeated as needed), then `Done { status }` |

The server parses a command's argv, so there is one grammar, one binary, and
no command encoding to version.

## CLI

Every pane gets `FUX_PANE=%N` and `FUX_SOCKET` in its environment. So a
command run inside a pane targets that pane without flags, like `TMUX_PANE`.
`-t` targets explicitly:

- a pane: `%N`;
- a tab: `@N`;
- a workspace: `+N` or its name. Not `$N`, which the shell would expand.

A command that needs a target and has neither `-t` nor `FUX_PANE` fails with
a message naming `-t`. It never guesses a "current" pane.

| Command | Does |
| --- | --- |
| `fux` / `fux attach [-t WS] [--nested]` | Attach, starting a server if none answers |
| `fux server [--socket P] [--config F]` | Run a server in the foreground |
| `fux kill-server` | Stop the server (hangs up every pane) |
| `fux ls [--json]` | Workspaces, tabs, panes, clients |
| `fux new-workspace [-n NAME] [-- CMD…]`, `fux new-tab [-t WS] [-n NAME] [-- CMD…]` | Create, with a shell; CMD, if given, is typed into it |
| `fux split -h\|-v [-t %N] [-- CMD…]` | Split, with a shell; CMD, if given, is typed into it |
| `fux kill-pane\|kill-tab\|kill-workspace [-t …]` | Close (no confirmation from the CLI) |
| `fux rename -t TARGET NAME` | Rename a pane, tab or workspace |
| `fux move-pane -t %N --to @N\|+N\|new-tab\|new-workspace`, `fux move-pane -t %N -L\|-R\|-U\|-D` | Move a pane (to a workspace: its first tab; with a direction: beside that neighbour, as `S-arrows` do) |
| `fux swap-pane -t %A %B`, `fux swap-pane -t %A -L\|-R\|-U\|-D` | Swap two panes, anywhere |
| `fux reorder pane\|tab\|workspace [-t TARGET] --next\|--previous` | Move one place in order (for the action menus) |
| `fux terminate [-t %N]` | SIGTERM to the pane's foreground process group, never its shell (the pane menu's "terminate") |
| `fux resize-pane -t %N -L\|-R\|-U\|-D [N]` | Adjust weights |
| `fux send-keys -t %N [-l] KEYS…` | Keys (`C-c`, `Enter`, …) or literal text |
| `fux capture-pane -t %N [-S -N] [--json]` | Screen text, optionally with history |
| `fux set OPTION VALUE`, `fux bind [-g GROUP] KEY CMD…`, `fux unbind KEY`, `fux unbind-all` | Change the running configuration |
| `fux reload` | Re-run the config file against the defaults |
| `fux list-buffers`, `fux show-buffer [-b N]`, `fux paste-buffer [-b N] [-t %N]` | Paste buffers |
| `fux detach [-c CLIENT]` | Detach a client |
| `fux list-keys` | Key names and the current bindings |

Some commands act on a client's screen rather than on shared state:
`command-column`, `choose-tab`, `choose-workspace`, `menu pane|tab|workspace`,
`command-prompt`, `copy-mode`, `rename-prompt`, `confirm-close`, `zoom`,
`select-tab`, `select-pane`, `select-workspace` (for `{` and `}`), and
`choose-pane` (the pane menu's "swap with…"). From a binding or the `:` prompt, they act on
the client that pressed the key. From the command line they need `-c CLIENT`
(`fux ls` lists clients). Without it they fail with a message naming the
flag.

Exit status 0 means done; 1 means the command failed, with the reason on
stderr; 2 means usage. `--json` output is for scripts and agents; its shape is
documented in the README, and changing it is a breaking change.

Key names, for `bind` and `send-keys`, are:
- the tmux ones: `C-x`, `M-x`, `S-Left`, `Enter`, `Tab`, `BTab`, `Escape`,
  `Space`, `BSpace`, `Up`, `Home`, `PageUp`, `F1`–`F12`, …;
- plus any single character.

`encode.rs`'s key set defines the full list, and `fux list-keys` prints it.

## Configuration

The config file is a list of fux commands, one per line, as in `tmux.conf`. It
lives at `--config`, else `$XDG_CONFIG_HOME/fux/fux.conf`, else
`~/.config/fux/fux.conf`, and is optional.

```sh
# ~/.config/fux/fux.conf
set prefix C-b
set shell /bin/zsh -l            # default: $SHELL, else /bin/sh
set history-lines 10000
set clipboard off                # default: write-only (OSC 52 on)
set buffers 16

unbind-all                       # optional: start from an empty key table
bind h split -h
bind v split -v
bind d detach
bind -g Tabs T choose-tab        # -g puts it under a command-column group
```

No dependency is needed, and it is the right format here anyway:

- There is **one grammar** for the CLI, keybindings, the `:` prompt and the
  config. A line is split into words like a shell (whitespace, `'…'` and
  `"…"` quoting, backslash escapes, `#` comments), and a binding's command is
  the rest of its line.
- **`set`, `bind` and `unbind` are ordinary commands**, so
  `fux bind x kill-pane` or `:set clipboard off` change a running server the
  same way the file does.
- **`fux reload`** runs the file again against the defaults. On any error it
  names the file and line and keeps the previous configuration whole, never
  half-applied.
- At startup, an invalid file does not stop the server. It runs on the
  defaults, logs the error, and shows it as a notice to each client that
  attaches, until a reload succeeds.
- `set` and `bind` inside the file affect only the configuration, not
  workspaces or panes. Layout commands in the config file are an error.
- The tokenizer is about 100 lines and gets its own tests.

TOML was the alternative. It needs `toml` and `serde` (with `toml`'s own
dependencies), and it would still need this command grammar for binding
values.

## Default keys (after `C-b`)

These are the current defaults. The prefix alone opens the command column,
which lists all of them.

- `[` `]` tabs; `{` `}` workspaces; Tab / Shift-Tab / Backspace pane focus;
  Alt-arrows directional focus;
- `t` new tab; `T` tab chooser; `w` new workspace; `W` workspace chooser;
- `p` `s` `S` pane, tab and workspace action menus;
- `h` `v` split; `z` zoom; `r` rename; `x` close (confirm `y`);
- Ctrl-arrows resize; Shift-arrows move;
- `c` copy/select mode; `P` paste the newest buffer; `:` command prompt;
  `d` detach;
- the prefix twice sends it to the pane.

## Not included

- **Mouse support of any kind**, by decision (see Input).
- **Saving and loading layouts**, by decision.
- **Watching the config file**: run `fux reload` instead.
- **Keybindings without the prefix** (tmux's `bind -n`).
- **Passing pane titles and bells to the outer terminal**, or the Kitty
  keyboard protocol.

## Dependencies

| Crate | Why |
| --- | --- |
| `fux-vt` (path, 0.1.1) | Emulator |
| `rustix` 1.x (`pty`, `termios`, `process`, `event`, `fs`, `net`, `stdio`) | PTYs, processes, poll, sockets, terminal modes |
| `signal-hook` | Signal → self-pipe (rustix does not install handlers) |
| `unicode-width` | Bar and overlay layout (already in the graph through fux-vt) |
| `libc` (macOS only) | `proc_listallpids` for session hangup, `getpeereid` for the peer check, `proc_pidinfo` for a new pane's working directory; rustix has none of them on macOS. Also `getsid` and `tcgetpgrp`: rustix builds a `Pid` from their result unchecked, and a result of 0 is undefined behaviour there (on Linux, where kernel threads have session 0, fux reads `/proc/PID/stat` instead) |

The lints stay as today: clippy forbids `unwrap`, `expect`, `panic!`,
`unreachable!`, `todo!` and `unimplemented!`, and warns on
`indexing_slicing`. CI runs clippy with `-D warnings` on macOS and Linux.
`unsafe` appears only for `pre_exec` and the macOS `libc` calls, each with a
`SAFETY:` comment. The toolchain pin stays in `rust-toolchain.toml`.

Written in fux instead of depended on, each small and tested:

- the command tokenizer and grammar (the CLI, the config, bindings and `:`);
- the `--json` writer (output only, about 80 lines);
- base64 for OSC 52 (about 20 lines).

Gone: 17 `bevy_*` crates, `nix`, `portable-pty`, `termina`, `hyper`,
`smol-hyper`, `http-body-util`, `ureq`, `async-io`, `async-channel`,
`parking_lot`, `ron`, `base64`, `serde` and `serde_json`. No build dependency
on ALSA, and no async.

## Testing

Ordinary tests, written with each part:

- **Unit tests, no PTY:**
  - the layout (rectangles, minimums, directional focus, collapse);
  - the input decoder (the sequences fux uses, a sequence split across
    reads, the Escape deadline);
  - encoding (ported with its tests);
  - render diffs (the diff applied to the old grid equals the new grid);
  - the protocol codec;
  - the command tokenizer and grammar, and the config (errors name their
    line, and a failed reload changes nothing);
  - copy/select motions and search over a known screen and history, and
    selection text (wide glyphs, wraps, blocks);
  - overlays (command column scrolling, choosers, menus acting on the item
    they were opened for).
- **Integration tests:** a real server, real PTYs, real `fux` CLI calls, and a
  scripted attach client, covering each milestone's usable end state. The
  fork race that corrupted the last suite's captures is avoided by opening
  PTYs and forking only under one lock.
- **CI:** the `verify` job only (fmt, clippy `-D warnings` and tests, on
  `macos-15` and `ubuntu-24.04`), without the ALSA step, the fux-fuzz and
  repro steps and the agent-exercise step. The smoke, trace, fuzz and
  property jobs go with the code they ran.

## Later, not in the first version

Hardening, once the functionality is in use:

- tests that each prove a lesson from `bevy-final:fux-fuzz/BREAKS.md` (for
  example a dash background job hung up on close (013), 3000 keys to a
  stopped program (021), a reused socket inode on ext4 (014), descriptor
  pressure (012)), each shown to fail against its bug;
- `cargo-fuzz` targets for the decoder, the protocol codec and the command
  tokenizer, and a black-box harness driving the CLI and attach clients;
- benchmarks (key-to-echo latency, heavy output, idle CPU, memory) against
  the Bevy version;
- Linux runs beyond CI (ext4 `/tmp`, amd64).

## Plan

0. Tag `bevy-final` at `e2b114f` and push the tag.
1. On branch `rewrite`:
   - delete the directories listed above;
   - make the workspace fux-vt plus a new `fux` crate at 0.13.0;
   - reduce the README to what exists;
   - trim CI to the `verify` job;
   - add the skeleton: CLI parsing, socket, protocol, and a server that
     answers `ls`;
   - open a draft PR from `rewrite` to `main`. CI runs only on pull
     requests to `main` (and on `main`), so without it no milestone is
     checked by CI.
2. One pane: spawn, attach, detach and reattach, render and resize.
3. The layout: splits, focus, resize, zoom, the bar.
4. Workspaces and tabs, and independent views for several clients.
5. The full CLI, with `--json`.
6. The config file, bindings, and the command grammar shared by all four
   surfaces.
7. Overlays: the command column, choosers, action menus, the `:` prompt,
   prompts and confirmations.
8. Copy/select mode, paste buffers, the clipboard.
9. Mark the PR ready. The user tries it by hand and decides when to merge it
   and when to publish 0.13.0.

Each step lands with its tests, and `main` keeps the Bevy version until that
PR is merged.

## Estimate

About 8–10k lines of Rust, including tests:

| Part | Lines |
| --- | --- |
| The server loop and panes | ~1.5k |
| Layout | ~0.6k |
| Input decoding and encoding | ~0.8k |
| Rendering and the bar | ~1k |
| Overlays (command column, choosers, menus, prompts) | ~1.5k |
| Copy/select mode and buffers | ~1k |
| The command grammar, CLI, config, protocol, socket and JSON | ~1.5k |
| The client | ~0.3k |

The current `src/` is 14.4k lines, not counting its Bevy dependencies.
