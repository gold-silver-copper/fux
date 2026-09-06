# fux

fux is a minimal persistent terminal multiplexer. Workspaces group related work, tabs switch
layouts, and splits show terminals together. One session server per user keeps pane processes
and their bounded history alive while viewers attach, detach and reconnect. The authoritative
model lives in a standalone [`bevy_ecs`](https://docs.rs/bevy_ecs/0.19.1/bevy_ecs/) World; the
viewer is a small terminal compositor. fux builds, installs and runs with no other program
present, opens no network listeners and needs no keys.

## Use

```sh
cargo build --release --locked
./target/release/fux
```

A fresh `fux` starts the session server on demand, creates the workspace `default` with the tab
`main` and one pane running your shell, and attaches. Detaching or closing the terminal leaves the
server and its panes running. Persistence means surviving detach: nothing is resurrected after
the server or the machine restarts.

- `fux` attaches to the workspace most recently attached by any viewer; on a fresh server it
  creates `default`. `fux NAME` opens or creates a named workspace. There is no startup picker.
- `fux workspace list`, `fux workspace new [NAME]`, `fux workspace kill NAME`.
- `fux bindings` prints the configured prefix and bindings by group.
- `fux serve --name NAME` runs the server in the foreground (SIGINT/SIGTERM shut it down).
- `fux attach --socket PATH` attaches to an explicit private attachment socket, for example one a
  koh gateway exposes.
- `fux [NAME] new|split|focus|kill|resize|send-keys|capture|list|tab|subscribe …` and
  `fux [NAME] ctl JSON` drive the workspace's control socket from scripts. `fux --help` lists them.

Configuration is `$XDG_CONFIG_HOME/fux/config.toml` (default `~/.config/fux/config.toml`); a
missing file means defaults. Every key is optional:

```toml
prefix = "C-a"                                  # one byte: a printable key, C-x, Esc, Space, DEL, 0xHH
default-command = { argv = ["/bin/zsh", "-l"] } # default: $SHELL -l, else /bin/sh -l
clipboard = "disabled"                          # or "write-only": OSC 52 copies reach the terminal
[bindings]                                      # key = action, merged over the defaults
"|" = "split-side"

[history]
scrollback-lines = 10000                        # per pane, 1-100000

[limits]
max-panes = 128                                 # per workspace
max-tabs = 32
max-workspaces = 64

[style]                                         # sixteen ANSI names, "default" (terminal foreground) or "none" (keep the cell's colour)
bar = "white"                                   # workspace name, inactive tabs, pane id: title
bar-background = "bright-black"                 # background of the bar row
tab-active = "default"                          # current tab (drawn reversed)
separator = "bright-black"                      # separators away from the focused pane
separator-focused = "default"                   # separators touching the focused pane (bold)
notice = "yellow"                               # transient notices; errors are always red
```

Private sockets live under `$XDG_RUNTIME_DIR/fux` (macOS fallback `~/Library/Caches/fux-runtime`);
daemon diagnostics go to `$XDG_STATE_HOME/fux/daemon.log` (default `~/.local/state/fux`).

If an older fux server (0.2.x, control preface `FUXCTL1`) still owns that directory, `fux` explains
the mismatch, lists the workspaces it recorded, and asks in the terminal: `k` stops the old server
after you type `stop` (this terminates its panes), `s` shows how to run the new version alongside it
in a separate runtime directory, `q` leaves it alone. Without a terminal it only reports and exits.

## Keys

Ordinary keys are byte-exact pane input. The prefix (Ctrl-A by default) enters command mode and
immediately shows the command column: a box in the bottom-right corner, directly above the bar,
one row per binding under its group heading, only as wide as its widest line and only as tall as
its content. Commands run at once; a burst such as prefix-`|` is applied before the next repaint,
so nothing flashes. Prefix twice sends one literal prefix. Unknown keys stay in command mode and
keep the column open; Esc leaves without sending anything. Dim rows are unavailable in the current
context and say why when pressed. Keys are matched without Shift: `x` and `X`, `|` and `\`, `-`
and `_` are the same key, so no two bindings may differ only by Shift. When the terminal is too
short for every row the column scrolls one row per `↑`/`↓` and a screenful per `PgUp`/`PgDn`,
with `▲ n more` / `▼ n more` rows marking what is hidden (on one or two rows it scrolls without
them). The tab and workspace choosers, the rename and new-workspace prompts and the
close confirmations use the same corner box. There is no command-mode timeout.

| Group | Keys | Action |
|---|---|---|
| Panes | `|` `-` | split side by side / stacked (new pane runs the default command and takes focus) |
| | `x` | close the focused pane after confirming with `y`; the target is the pane you pressed on |
| | `r` | resize mode: arrows or `h j k l` adjust repeatedly, Enter finishes, changes are kept |
| | `[` | history and copy mode for the focused pane |
| Focus | `h j k l` | move focus by direction |
| Tabs | `t` `n` `p` | new tab, next, previous |
| | `w` `,` `c` | choose tab, rename the current tab, close the current tab (confirmed) |
| Workspaces | `s` `a` | choose a workspace, add one (optionally named) and switch to it |
| Session | `d` | detach |

The bottom row is always the bar, on its own background: the workspace name, the tabs with the
current one reversed, and the focused pane as `id: title` on the right (or its exit status once
it has exited). Transient
notices such as copy results, errors and workspace switches appear in that right zone for two
seconds or until the next key. Panes have no frame: adjacent panes share one thin separator, drawn
bold next to the focused pane, and a single pane fills everything above the bar. Colours are muted
by default and configurable through `[style]`. Pane sizes are negotiated over the smallest viewer
showing the tab, so two viewers with different terminals see the same pane contents; the larger
viewer leaves unused margins. On a tiny terminal the column first scrolls and then truncates labels with `…`; with a single row only
the bar is shown. A pane that exits while unfocused keeps a dim
`exit N` marker in its last row; the focused pane's status is in the bar.

Copy mode (`[`) browses the pane's private history: arrows or `h j k l` move, `u`/`d` and
PgUp/PgDn scroll, Space starts a selection, `y` or Enter copies it and returns to live output, `g`
jumps back to live output, `q` leaves, Esc clears the selection first and then backs out. New
output never moves another viewer's viewport or changes a selection silently: if eviction or a
resize invalidates the selected rows, the selection is cleared with a visible notice.

Mouse: when the application under the pointer does not request mouse input, the wheel browses
that pane's history and dragging selects text inside the pane. When it does, events reach the
application with pane-relative coordinates. Hold Shift to force fux's own history/selection
handling; the keyboard path above always works for terminals that reserve gestures. Copies use
the configured clipboard policy (`disabled` by default; `write-only` emits one bounded OSC 52
sequence of at most 1 MiB encoded) and report success or the reason for failure.

Escape is both a key and the start of many sequences. A lone Esc is forwarded after a short
disambiguation window (35 ms) unless more bytes arrive; sequences split across reads are
reassembled, and a cancelled mode keeps ownership of an unfinished paste until it drains.

## Panes and history

Every pane keeps up to `scrollback-lines` rows of history in the server while hidden, on another
tab, or detached; switching tabs or workspaces and reattaching never discards it. Older rows are
evicted first. Closing a pane, closing a tab or killing a workspace frees the history. There is no
merged output log.

When the only pane of the only tab exits by itself the workspace retires with that exit status:
attached viewers see the final screen, then exit with the code; the server finalizes once the
viewers have seen it (or after five seconds). Other natural exits close the pane, and an emptied
tab closes. Confirmed close and `kill` send SIGHUP to the pane's process group, SIGKILL after one
second, and reap it. Workspace kill and server shutdown do the same for every pane.

## Working with koh and zor

fux composes with the independently built koh and zor programs through versioned process
protocols; it never links, spawns or supervises them.

Remote access is koh's job. On the machine running fux:

```sh
koh gateway serve --socket "$XDG_RUNTIME_DIR/fux/default.attach.sock" --allow CLIENT_ID
```

and on the viewer machine:

```sh
koh gateway connect SERVER_ID --socket /private/dir/fux.sock
fux attach --socket /private/dir/fux.sock
```

koh authenticates the peer before it opens the local socket, carries the opaque attachment
stream, and resumes it across transient link loss without repeating input. Stopping either side
leaves the panes and local attachments untouched.

Observation is zor's job. Point it at a pane's workspace control socket:

```sh
zor observe --socket "$XDG_RUNTIME_DIR/fux/default.sock" --pane 1 --pid PID
```

zor negotiates the control preface, samples `list` and `capture`, and runs its own rules. A
missing, stalled, crashed or malformed observer cannot block or change a pane. The socket paths,
identities and events zor consumes are documented in the [control protocol](docs/local-control-protocol.md).

## Verification

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked -- --test-threads=1
cargo doc --no-deps --locked
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked
tests/verify/release-package.sh
```

Deterministic ECS tests inject events and time (`tests/ecs.rs`, including randomized command
sequences); real-process scenarios use disposable HOME/XDG directories and owned processes only
(`tests/local_cli.rs`, the fixture-child suite). The optional cross-repository job and
`python3 tools/dependencies.py verify --build` rebuild koh and zor from pinned bases plus the
patches in `dependency-patches/` and run the required real koh and real zor integrations with
explicit binary paths; set `ZOR_BIN` and `FUX_REQUIRE_ZOR_BIN=1`, or `FUX_BIN` and
`KOH_REQUIRE_FUX_BIN=1`, so they can never silently skip.

## Documents

- [docs/design.md](docs/design.md): architecture, entity model, system order, lifecycle.
- [docs/ecs-plan.md](docs/ecs-plan.md): the plan written before the rewrite.
- [docs/ecs-acceptance.md](docs/ecs-acceptance.md): requirement-by-requirement acceptance audit.
- [docs/local-attachment-protocol.md](docs/local-attachment-protocol.md) (v5) and
  [docs/local-control-protocol.md](docs/local-control-protocol.md) (`FUXCTL2`).
- [docs/security.md](docs/security.md), [docs/release-readiness.md](docs/release-readiness.md),
  [CHANGELOG.md](CHANGELOG.md), [HANDOFF.md](HANDOFF.md).
- Everything else under `docs/` and the `*-prompt.md` files are historical records of earlier
  architectures and are labelled as such.

Licensed under MIT. Terminal handling reused from earlier fux releases and the koh/zor projects
retains attribution in [LICENSES](LICENSES).
