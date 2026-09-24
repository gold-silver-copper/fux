# fux without Bevy: design

Status: proposal. Nothing here is implemented yet.

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
| Mouse and copy mode | Yes, in the first version. |
| Control surface | The `fux` CLI only. No RPC, HTTP or JSON-RPC. |
| Bevy | None. No async runtime either. |
| System calls | `rustix`, not `nix`. `portable-pty` is replaced by rustix's PTY API. |

## What stays, what goes

**Stays, unchanged:** `fux-vt`, published as 0.1.1. It is the emulator, with
no dependency but `unicode-width`, and koh depends on it. The branch
`feat/fux-vt-for-koh` also stays: koh pins fux-vt to it by git, and koh is not
touched.

**Ported (logic and tests, not files):**

| From | What | Bevy/nix today |
| --- | --- | --- |
| `src/encode.rs` | Key and mouse encoding for panes (application cursor, SGR mouse) | none |
| `src/paste.rs` | Bracketed-paste decoding, bounded to 64 KiB | trivial |
| `src/transport.rs` | Socket path rules, 0700 directory checks, lock, stale-socket replacement, inode pin (014) | nix → rustix |
| `src/terminal.rs` | Session hangup on close (013), stopped ≠ exited (020), byte-bounded input (021), bounded drain, EINTR retry (010) | nix/portable-pty → rustix |
| `src/selection.rs` | Copy-mode selection over fux-vt row IDs | light |
| `src/chrome.rs` | The bottom bar | light |

**Deleted:** `src/`, `tests/`, `fux-fuzz/` (a BRP-driven harness), and
`fux-agent-exercises/` (BRP-driven agent campaigns). Also deleted:
`verification/` and the BRP-specific docs. They stay retrievable at the tag
`bevy-final` (`f86dee7`). `fux-fuzz/BREAKS.md` and the repro scripts are the
record of mistakes not to repeat. The lessons that still apply become tests
here, listed under Testing.

## Architecture

```
fux (one binary)
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

### Panes and processes

A pane owns:

- a PTY: `openpt(RDWR | NOCTTY | CLOEXEC)`, `grantpt`, `unlockpt`,
  `ptsname`, `tcsetwinsize`;
- a `fux_vt::Parser`;
- a child process, started through `std::process::Command` with the slave as
  stdio. A `pre_exec` hook calls `setsid` and `ioctl_tiocsctty`, both
  async-signal-safe.

Each pane also has an ID, `%N`, which only increases, and a name.

Process lifecycle, from the lessons:

- **Exit:** on SIGCHLD, every live pane is probed with
  `waitid(Pid, EXITED | NOHANG | NOWAIT)`. Only an exited, killed or dumped
  status counts as an exit. macOS also reports stops here (020), so a stop is
  ignored. The status is kept, and the pane stays with its last screen.
- **Close:** SIGHUP to the leader's group and to every process in its session.
  The session is found through `/proc` on Linux and `proc_listallpids` on macOS
  (013), so a job started by dash survives only if it ignores the hangup, as
  under a real terminal. After 100 ms, close the master, SIGKILL the group,
  and reap.
- **Input:** a per-pane queue bounded by bytes (each piece costs its length
  plus 64; 16 × 64 KiB in total). It is never bounded by piece count (021).
- **Output:** read into the parser, with terminal replies (DSR, DA) appended
  to the same bounded input queue.

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
  closing its last reference ends it.

### Independent views

Each attached client has its own view:

- its size, and its current workspace;
- per workspace, the selected tab; per tab, the focused and last-focused pane;
- zoom, and scrollback per pane;
- its mode: normal, prefix, prompt, confirm, copy (with selection) or help;
- its last sent frame, used for diffing.

A PTY's size is the smallest rectangle it has among the clients currently
showing it, with a 1×1 minimum. A pane nobody shows keeps its last size.
Every change is published at once. There is no "next update".

### Input

The client is a dumb pipe. It puts the outer terminal in raw mode, enables
SGR mouse (1000/1002/1003/1006), bracketed paste and focus events, and
forwards raw bytes. The server decodes them, per client:

1. **Decoding:** keys (CSI/SS3, UTF-8, a lone Escape after 35 ms), SGR mouse,
   and bracketed-paste envelopes, bounded as before.
2. **Routing by mode:**
   - Prefix, prompt, confirm, copy and help modes own their input.
   - The prefix key twice sends it literally.
   - Pastes never become commands.
   - Otherwise the input goes to the focused pane, re-encoded for that pane's
     modes by the ported `encode.rs`.
3. **Mouse:**
   - Click to focus; click a tab in the bar to select it.
   - Wheel and drag scroll or select, unless the application asked for the
     mouse. Shift overrides that.
   - Application mouse events get pane-relative coordinates.
   - Separators and the bar never click through to a pane.

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
3. If a client's socket stops draining, stop queueing paints for it. Once it
   drains again, send one full repaint. A slow client never grows the server
   and never stalls the loop.

### Copy mode and clipboard

- Keyboard copy mode (`h/j/k/l`, `u/d`, Space, `y`/Enter, `q`/Esc) and mouse
  drag selection, over fux-vt row IDs, ported from `selection.rs`.
- Copying emits OSC 52 to the client that copied. It is off by default and
  enabled by `clipboard = "write-only"`, bounded as today. Nothing ever reads
  the clipboard.

### Security

A socket with the same rules as today:

- `FUX_SOCKET`, else `$XDG_RUNTIME_DIR/fux/server.sock`, else
  `$TMPDIR/fux/server.sock`.
- An owned 0700 directory, a 0600 socket, and a lock file.
- A stale socket is replaced only if nothing answers.
- The inode is pinned for cleanup (014).

Also:

- The peer's UID is checked on accept: rustix's `socket_peercred` on Linux,
  `getpeereid` through `libc` on macOS.
- Every message is length-prefixed with a hard size cap (007/008). Reads
  retry on `EINTR` (010).
- The whole surface is the fixed command list below. There is nothing like
  "despawn any entity".

## Protocol

It is private between one `fux` binary and itself. Frames are
`u32 length | u8 kind | payload`, with a 1 MiB cap. The first frame is `Hello`
with the exact binary version; any mismatch is refused with "server is
version X; restart it".

| Direction | Frame |
| --- | --- |
| client → server | `Hello { version, role }` |
| client → server | `Attach { rows, cols, workspace? }` |
| client → server | `Input(bytes)`, `Resize { rows, cols }`, `Detach` |
| client → server | `Command { argv, cwd, pane? }` |
| server → client | `Paint(bytes)`, `Exit(reason)` |
| server → client | `Result { status, stdout, stderr }` |

The server parses a command's argv, so there is one grammar, one binary, and
no command encoding to version.

## CLI

Every pane gets `FUX_PANE=%N` and `FUX_SOCKET` in its environment. So a
command run inside a pane targets that pane without flags, like `TMUX_PANE`.
`-t` targets explicitly:

- a pane: `%N`;
- a tab: `@N`;
- a workspace: `$N` or its name.

| Command | Does |
| --- | --- |
| `fux` / `fux attach [-t WS]` | Attach, starting a server if none answers |
| `fux server [--socket P] [--config F]` | Run a server in the foreground |
| `fux kill-server` | Stop the server (hangs up every pane) |
| `fux ls [--json]` | Workspaces, tabs, panes, clients |
| `fux new-workspace [-n NAME]`, `fux new-tab [-t WS] [-n NAME]` | Create |
| `fux split -h\|-v [-t %N] [-- CMD…]` | Split, optionally running a command |
| `fux kill-pane\|kill-tab\|kill-workspace [-t …]` | Close (no confirmation from the CLI) |
| `fux rename -t TARGET NAME` | Rename a pane, tab or workspace |
| `fux move-pane -t %N --to @N\|$N\|new-tab`, `fux swap-pane -t %A %B` | Rearrange |
| `fux resize-pane -t %N -L\|-R\|-U\|-D [N]` | Adjust weights |
| `fux send-keys -t %N [-l] KEYS…` | Keys (`C-c`, `Enter`, …) or literal text |
| `fux capture-pane -t %N [-S -N] [--json]` | Screen text, optionally with history |
| `fux reload` | Re-read the config |
| `fux detach [-c CLIENT]` | Detach a client |

Exit status 0 means done; 1 means the command failed, with the reason on
stderr; 2 means usage. `--json` output is for scripts and agents.

## Configuration

A TOML file, `--config`, else `$XDG_CONFIG_HOME/fux/config.toml`, else
`~/.config/fux/config.toml`. It is optional; missing fields keep their
defaults.

```toml
prefix = "C-b"
shell = ["/bin/sh"]       # default: $SHELL, else /bin/sh
history_lines = 10000
clipboard = "off"         # or "write-only"

[bindings]                # after the prefix; replaces the default table when present
h = "split -h"
v = "split -v"
d = "detach"
```

A binding's value is a CLI command line. Keybindings and the CLI therefore
share one command set, and there is no second action vocabulary. `fux reload`
applies a changed file, and an invalid file keeps the previous one.

## Default keys (after `C-b`)

These are the current defaults, less the choosers and context menus (deferred,
below):

- `[` `]` tabs; `{` `}` workspaces; Tab / Shift-Tab / Backspace pane focus;
  Alt-arrows directional focus;
- `t` new tab; `w` new workspace; `h` `v` split; `z` zoom; `r` rename;
  `x` close (confirm `y`);
- Ctrl-arrows resize; Shift-arrows move;
- `c` copy mode; `y` copy visible; `?` help (a read-only list of bindings);
  `d` detach.

## Deferred

Not in the first version:

- the command column with navigation;
- tab and workspace choosers;
- right-click context menus;
- config hot reload by file watching (use `fux reload`).

Everything those did is reachable through bindings or the CLI. They can come
back later as overlays drawn by the same renderer.

## Dependencies

| Crate | Why |
| --- | --- |
| `fux-vt` (path, 0.1.1) | Emulator |
| `rustix` 1.x (`pty`, `termios`, `process`, `event`, `fs`, `net`, `stdio`) | PTYs, processes, poll, sockets, terminal modes |
| `signal-hook` | Signal → self-pipe (rustix does not install handlers) |
| `unicode-width` | Bar and overlay layout |
| `toml` + `serde` | Config |
| `serde_json` | `--json` output only |
| `lexopt` | Argument parsing, small and dependency-free |
| `libc` (macOS only) | `proc_listallpids` for session hangup, `getpeereid` for the peer check; rustix has neither on macOS |

Gone: 17 `bevy_*` crates, `nix`, `portable-pty`, `termina`, `hyper`,
`smol-hyper`, `http-body-util`, `ureq`, `async-io`, `async-channel`,
`parking_lot`, `ron` and `base64` (OSC 52 base64 is about 20 lines). No build
dependency on ALSA; no async.

## Testing

- **Unit tests, no PTY:**
  - the layout (rectangles, minimums, directional focus, collapse);
  - the input decoder (every sequence, split at every byte, the Escape
    deadline);
  - encoding (ported with its property tests);
  - paste bounds;
  - render diffs (the diff applied to the old grid equals the new grid);
  - the protocol codec (caps, truncation);
  - the config.
- **Integration tests:** a real server, real PTYs, real `fux` CLI calls, and a
  scripted attach client. The fork race that corrupted the last suite's
  captures is avoided by opening PTYs and forking only under one lock.
- **Lessons as tests:** hangup of a dash background job (013); stopped ≠
  exited (020); 3000 keys to a stopped program all arrive (021); a signal
  during attach (010); descriptor pressure (012); an oversized frame is
  refused (007/008); a reused socket inode is not deleted (014, ext4 in CI).
- **Fuzzing:**
  - `cargo-fuzz` targets for the decoder and the protocol codec;
  - later, a black-box harness driving the CLI and attach clients, as fux-fuzz
    did over BRP.
- **CI:** the current workflow minus the ALSA step, the agent-exercise job and
  the BRP repro table.

## Plan

0. Tag `bevy-final` at `f86dee7` and push the tag.
1. On branch `rewrite`:
   - delete the directories listed above;
   - make the workspace fux-vt plus a new `fux` crate at 0.13.0;
   - reduce the README to what exists;
   - trim CI;
   - add the skeleton: CLI parsing, socket, protocol, and a server that
     answers `ls`.
2. One pane: spawn, attach, detach and reattach, render and resize.
3. The layout: splits, focus, resize, zoom, the bar.
4. Workspaces and tabs, and independent views for several clients.
5. The full CLI, with `--json`.
6. Input modes: prefix, bindings, prompt, confirm, help.
7. Copy mode, the clipboard, the mouse.
8. The lesson tests, and the fuzz targets.
9. Merge `rewrite` into `main` when it is usable daily; publish 0.13.0.

Each step lands with its tests, and `main` keeps the Bevy version until
step 9.

## Estimate

About 6–8k lines of Rust, including tests: the server loop and panes (~1.5k),
layout (~0.6k), input decoding and encoding (~1k), rendering and bar (~1k),
copy mode (~0.6k), CLI, protocol, socket and config (~1.2k), and the client
(~0.3k). The current `src/` is 14.4k lines, not counting its Bevy
dependencies.
