# The Shape of fux

An agent workspace: a small native terminal multiplexer built directly on koh's per-pty emulator and
mosh-grade peer-to-peer attach path, with herdr's agent-state detection running natively on the
panes. One `cargo install`-able binary, one process, no sandbox.

- **Status:** proposal, audited against source. Supersedes the zellij-based draft of 2 Sep 2026.
- **Date:** 3 Sep 2026
- **Reference trees:** `references/` — koh 0.11.0 (7d1f514) · herdr 0.8.2 (94f6d9c,
  github.com/herdrdev/herdr) · zellij `main` at 0.46.0 (af38660), kept for grid edge cases only
- **Published:** koh **0.11.0** (crates.io, 3 Sep 2026, tag `v0.11.0`; fux pins `=0.10.0` until the
  first generic-host code lands, then `=0.11.0`, `default-features = false`, `backend-termina`) · fux 0.1.0 placeholder. herdr proper is not published; the `herdr`
  crate on crates.io is unrelated.

---

## Why not zellij

The previous draft embedded fux in zellij: fux as the binary, zellij's client and server as library
crates, detection as a wasm plugin, koh hosting zellij's client in a pty. It was buildable, and every
one of its risks came from the seam:

| Cost of the zellij route | Native route |
|---|---|
| Detection interpreted on wasmi, on a copy of pane text, behind a permission cache hack | Detection reads the `vt100::Screen` fux already owns |
| Two vt100 passes per frame: zellij emulates each pane, koh re-emulates zellij's output | koh's emulators *are* the panes; one compositing pass |
| Exact pins on crates that exist to serve one binary; 1.95 toolchain floor; committed wasm artifact; CI wasm target; a pipe protocol | None of it |
| The one risk that could sink the design: zellij's client rendering cleanly under koh's grid | Nothing renders under anything |
| Phone alerts via bell or title tricks through the plugin pipe | State is native; the phone gets a field on the existing diff |

What zellij offered that fux does not need: floating panes, KDL layouts, a plugin system, mouse
resize, session manager UI, years of grid polish. What fux gives up: that polish, and users' zellij
muscle memory. An agent workspace is a fixed set of long-running CLIs in splits with a status line.
That is a small multiplexer, and koh already contains most of one.

**What koh already is.** A pty spawner, a server-side `vt100::Parser` per pty with title, bell,
clipboard and exit-code callbacks, a diff engine that ships screen deltas, a state-sync protocol that
survives disconnects, session retention, identity and allow-lists, a phone client, and predictive
echo. That is one pane of a multiplexer plus the entire transport. koh's terminal, server, client and
pty layers total about 5.6k lines; zellij's pane and tab code alone is over 32k.

---

## Architecture: one binary at both ends, the workspace is the synced state

```
  ANY TERMINAL (fux client)                          HOST (fux server)
  ┌──────────────────────────────┐   iroh QUIC     ┌───────────────────────────────────────┐
  │ WorkspaceState (replica)     │ <─datagrams──── │ WorkspaceState (authoritative)         │
  │  layout · tabs · focus ·     │  WorkspaceDiff: │  layout · tabs · focus · status ·       │
  │  status · per-pane grids     │  changed pane   │  per-pane grids ← Pty + ServerTerminal │
  │            │                 │  cells, layout  │            ▲              ×N            │
  │  compositor (ratatui)        │  edits          │  input router · detection · notifier   │
  │  + prediction on focused pane│ ─keys, resize─> │  control socket · workspace manager     │
  │            │ Buffer diff     │                 └───────────────────────────────────────┘
  │  real terminal (KohBackend)  │                    koh: ssp · transport_iroh · pty ·
  └──────────────────────────────┘                    terminal · predict — generic over state
```

The same binary runs on the phone, the laptop and the host. What crosses the network is not a
screen but the workspace: a layout tree per tab, the focused pane, status segments, and for each
pane the cells of its visible viewport. koh's `Transport<Local, Remote>` is already generic over
any `SyncState`, so this is a new state type with a diff, not a new protocol.

| Layer | Runs on | Built from | New code |
|---|---|---|---|
| **Panes** | host | koh `pty::Pty` + `terminal::ServerTerminal`, one pair per pane. Bytes in, `vt100::Screen` out, with OSC 0/1/2/52/9;4 and BEL captured by callbacks. | none |
| **Workspace** | host | `WorkspaceState` and its diff; BSP layout ported from herdr; tabs; input router decoding keys and SGR mouse; scroll and copy mode viewports; detection; notifier; control socket; workspace manager. | ~5k lines |
| **Transport** | both | koh `ssp::Transport`, `transport_iroh`, session retention, allow-lists, keys, made generic over the synced state and the host on the server side and the renderer on the client side. | koh 0.11.0 (shipped) |
| **Client** | any terminal | ratatui compositor over the replica: panes as widgets, borders, tab bar, status line, popups; ratatui's `Buffer` diff drives the real terminal through a `Backend` over koh's `KohBackend`; koh's predictor on the focused pane's grid. | ~1.5k lines |

### Why the workspace, not the screen, is what syncs

Three tiers were on the table. Escape bytes into a `vt100::Parser` (what a pty does) throws
ratatui's diff away and rebuilds it. A composited cell grid as the state removes the parse round
trip but still resends every cell that moves when a split closes or a tab switches. Syncing the
workspace itself sends only the cells of the panes that changed; layout edits are a few bytes; a
tab switch to already-seen panes sends nothing. It also lets each client fit the layout to its own
terminal, which is the only way a phone shows a laptop-sized workspace sensibly. ratatui's buffer
diff then does exactly what it exists for: minimal repaint of a real terminal.

`WorkspaceState`: tabs (each a layout tree of pane ids, with zoom), focus, status segments, title,
bell count, popups as floating panes, and `panes: map<PaneId, PaneView>` where `PaneView` is the
visible rows of the pane at its current viewport (scrolled or live), cursor, modes and agent state.
`WorkspaceDiff` is layout edits plus per-pane changed-cell runs against the base, with a full pane
grid when the base does not have that pane at that size. `SyncState` needs `diff_from`, `apply`,
`PartialEq`, a decode limit and a resource budget; the budget is cells summed over panes, as
`TerminalScreen`'s is today.

### What koh has to become generic over

- **Server:** `serve` takes a host that produces the state. Today's host is a pty whose
  `ServerTerminal` snapshot is a `TerminalScreen`; fux's host is the workspace manager whose
  snapshot is a `WorkspaceState`. The host trait is `snapshot() -> S`, `input(&[u8])`,
  `resize(rows, cols)`, `changed: Notify`, `exited() -> Option<u32>`. `run_attached`'s snapshot
  gating, input coalescing and reaping apply unchanged.
- **Client:** `connect` takes a renderer for the remote state. Today's renderer is `render.rs`
  painting a `TerminalScreen` cell by cell; fux's is the ratatui compositor. `KohBackend` stays the
  terminal seam.
- **Prediction:** `predict.rs` reads cells from a `vt100::Screen`; it moves to a cell-reader trait
  so it can run over the focused `PaneView`.
- **Shared sessions:** all authorized peers attach to the same host instead of one session per
  endpoint id, with a per-client viewport size reported back so the host can size ptys. Pane sizes
  follow the workspace's own geometry, chosen by the host from the attached clients (last resize
  wins for v1); a smaller client clips or zooms.

The pty path keeps `TerminalScreen` and today's `koh` binary unchanged. Everything above is
additive and shipped as koh 0.11.0 (3 Sep 2026): `serve_with`/`connect_with` with the state type
selected by ALPN, `SessionHost`/`HostProvider`/`SharedHost`, `ClientState`, `predict::ScreenView`,
`ServerTerminal::progress()`/`take_unhandled_oscs()`, and `ConnectConfig::bell_command`.

### Local attach is remote attach over loopback

`fux` finds or starts the server, then attaches over iroh on the local machine. One code path for
local and remote; detach, reconnect and prediction come free. A unix-socket channel behind the
`IrohChannel` seam is a later optimisation. The server daemonises on first `fux`; `fux serve` in the
foreground is the same server under a supervisor.

---

## Surface: what the binary does

```sh
fux [name]                   # attach to a workspace, creating it and the server if needed
fux serve [--allow <id>]     # run the server in the foreground; --allow exposes it to a peer
fux connect <id>             # attach to a remote workspace, mosh-style (koh connect)
fux id                       # print this machine's endpoint id (koh id)
fux key passwd | info        # identity key management (koh key)
fux ctl <command> [args…]    # drive the running workspace over the control socket (see Control)
fux new | split | focus | …  # the common control commands, also exposed as top-level verbs
```

`id`, `key` and `connect` are koh's `run_id(IdConfig)`, `keycmd::run(KeyConfig)` and
`connect(ConnectConfig)` behind fux's own clap layer. `serve` is koh's `serve(ServeConfig)` with the
host set to the in-process workspace. Everything else is a thin client for the control socket.

Inside a workspace, a tmux-style prefix (default `Ctrl-a`) then: `|` `-` split, `hjkl` focus,
`x` close, `c` new pane, `t` new tab, `n` `p` next and previous tab, `z` zoom, `[` scroll and copy
mode, `d` detach, `s` workspace picker, `?` help. Mouse click focuses a pane; wheel scrolls the
pane under the cursor; Shift-drag selects text. Everything else goes to the focused pty verbatim. Configuration
is a TOML file with the bindings and the default command; there is no layout language.

---

## Control, not plugins

fux has no plugin system. What multiplexer plugins do splits into four capabilities: observe
(events, pane text), act (split, focus, send keys), draw (status segments, pickers, overlays), and
intercept input (modal keys, remapping). zellij's wasm sandbox exists to hand those four to untrusted
third-party code safely and portably, which is right for a project with a plugin ecosystem and wrong
for a personal tool where every author is you or an agent you are running. herdr's answer, a socket
API driven from its CLI, is the one fux adopts.

The line is drawn by latency: **whatever must be synchronous with input or rendering lives in
core; everything else is a process.** In core: the prefix key table, scroll and zoom modes,
detection (it runs on every pane drain and wants the screen without a copy), the compositor, the
status line renderer. Outside: anything that can be a command plus an event subscription. A program
that wants a UI runs in a pane or a popup and gets a real terminal, which is more than any plugin
drawing API offers.

### The socket

A unix socket, mode 0600, at `$XDG_RUNTIME_DIR/fux/<workspace>.sock`, speaking newline-delimited
JSON. Every message carries an `id`; the server answers with the same `id`. Two message kinds:

**Commands** (request, one reply):

| Command | Effect |
|---|---|
| `new [--cwd DIR] [-- argv…]` | open a pane; returns its id |
| `split <h\|v> [--target ID] [-- argv…]` | split a pane and run a command in the new half |
| `focus <ID\|left\|right\|up\|down>` | move focus |
| `zoom [ID]` | toggle the focused (or given) pane filling the screen |
| `kill <ID>` | close a pane and its process |
| `resize <ID> <+n\|-n>` | grow or shrink along the split axis |
| `send-keys <ID> <bytes>` | write to a pane's pty; keys as text or as `\x1b` escapes |
| `capture <ID> [--attrs] [--scrollback N]` | pane text, plain or with cell attributes, optionally with history |
| `tab <new\|next\|prev\|N>`, `workspace <list\|new\|kill>` | tab and workspace management |
| `list` | workspaces, tabs, and panes with id, command, pid, cwd, title, agent, state, geometry, focus |
| `set-status <segment> <text>` | write a named status-line segment; empty text removes it |
| `popup [--size WxH] -- argv…` | run a program in a centred overlay pane until it exits |
| `subscribe [events…]` | turn this connection into an event stream (below) |

**Events** (stream on a subscribed connection, one JSON object per line):

| Event | Payload |
|---|---|
| `pane.opened`, `pane.closed` | id, command, exit status on close |
| `pane.focused` | id |
| `pane.title` | id, title |
| `agent.state` | id, agent, old state, new state, timestamp |
| `pane.output` | id; rate-capped to one per pane per 250 ms; a nudge to `capture`, not the data |
| `workspace.resized` | rows, cols |
| `client.attached`, `client.detached` | endpoint id or `local` |

An agent that must wait until another agent is genuinely blocked is one `subscribe agent.state`
and a filter. A session picker is `popup -- fzf` fed by `list`. A "notify me on Slack" companion is
a twenty-line script.

### Bindings and hooks

The TOML config binds prefix keys to either built-ins or external commands. An external command
runs with `FUX_PANE`, `FUX_SOCKET` and `FUX_CWD` in its environment, so a binding is the same
program a user would run from a shell. `hooks` is a list of commands the server starts on boot and
restarts on exit; that is herdr's daemon shape without a separate concept, for people who want a
long-lived companion.

### What this costs

A companion process can be slow or dead, so the core must be complete without it. fux with nothing
attached to the socket is fully usable, which is not true of a zellij layout that references a
plugin that failed to load. Remote viewers over koh receive the screen and send keys; they never see
the socket. If the phone ever needs control it goes through a binding, not a network API.

---

## Detection: herdr's model, on the pane's own screen

Each pane's `ServerTerminal` already yields, on every drain, the live `vt100::Screen`, the OSC 0/2
title, and the bell count. That is the input herdr's detectors consume. herdr classifies each pane as
**working**, **blocked** or **idle** using per-agent TOML manifests (21 of them, `amp.toml` to
`qwen.toml`) of prioritised regex rules scoped to named regions: twelve fixed regions
(`whole_recent`, `osc_title`, `osc_progress`, `prompt_box_body`, `above_prompt_box`,
`last_non_empty_above_prompt_box`, `after_last_horizontal_rule`, and the prompt-marker family) plus
the parameterised `bottom_lines(n)` and `bottom_non_empty_lines(n)`. The Claude manifest uses nine
of them and carries rules keyed on the braille spinner range, the `esc to interrupt` footer, OSC 9;4
progress, and negative guards so that a user typing "do you want to proceed?" cannot impersonate a
state change.

The manifests are data and port verbatim, with Apache-2.0 attribution. The evaluator is rewritten,
not ported: herdr's is 3.2k lines (`manifest.rs` 1.5k, `detect/mod.rs` 1.6k) over its own
libghostty-vt screen type, plus 1k of manifest-update code fux does not need. A fresh evaluator over
`vt100::Screen::rows()` and the title, validated against herdr's manifests with captured-pane
fixtures, is the plan. Agent identification uses the pane's child process name via the pty's pid,
as herdr's `identify_agent` does.

Debounce is inherited, not rediscovered: a working-to-idle transition is confirmed three times at
100 ms, capped at 700 ms, with a 3 s startup grace window. That hysteresis is most of the difference
between a status indicator people trust and one they learn to ignore.

Detection runs on the server on each pane drain, natively. Cost is regex over the bottom-*n* lines
of the pane that changed, not every pane every frame.

One gap: vt100 0.16.2's `Callbacks` has no hook for OSC 9;4 progress. koh's callbacks would need an
`unknown_osc` passthrough or fux parses the raw drain for `ESC ] 9;4`. Rules on `osc_progress` are
disabled until then.

---

## Notifications

State transitions are native events on the server, so:

- **Desktop:** the notifier calls `notify-send` on Linux and `terminal-notifier`, falling back to
  `osascript`, on macOS, the two paths herdr uses. Runs in the server process, which is where the
  panes are, regardless of whether anyone is attached.
- **Phone:** the fux client holds the workspace replica, agent state included, so it notifies
  directly: `termux-notification` on Termux, the same notifier module as the host elsewhere. No
  bell or title tricks. Plain `koh connect` users still get the bell hook.
- **Status line:** every pane shows agent and state; blocked panes are highlighted; the tab-less
  v1 status line is one row.

---

## Distribution: one crate, one build, nothing to embed

- `cargo install fux` builds the whole thing everywhere: koh's library tree, the tiler, detection
  and the control socket. No features to choose, no wasm, no committed artifacts, no
  `include_bytes!`. A phone and a desktop run the same binary and can each host a workspace or
  attach to the other's.
- On Termux the build is koh's plus ratatui-core and the tiler, all pure Rust. koh's install notes
  (`pkg install rust clang pkg-config`) and its Android test suites apply directly. Termux ships
  rust 1.98, above koh's 1.91 floor.
- Prebuilt `aarch64-linux-android` binaries are a convenience once fux is public; `cargo install`
  is the same build.

The toolchain floor is koh's 1.91 (iroh 1.0). fux declares the same `rust-version`.

---

## Licensing

| Component | License | Use in fux |
|---|---|---|
| koh | MIT (0.10.0) | library dependency |
| herdr | Apache-2.0 | manifests vendored; `layout.rs` BSP tree ported; hysteresis constants and region vocabulary reimplemented |
| zellij | MIT | reference only |
| fux | MIT | |

Apache-2.0 code and data in an MIT crate needs attribution and a NOTICE entry; nothing else.
herdr's libghostty-vt bindings and ratatui UI are not used.

---

## Risks

### vt100 fidelity is load-bearing for every pane — *tested, retired*

Every agent CLI renders through koh's `vt100`. Tested 3 Sep 2026 with plain koh from a phone:
Claude Code renders correctly. Where a later CLI falls short, koh owns the pin and can patch or
fork vt100; it is a 5k-line crate.

### Input decoding does not exist in koh — *build it in the router, host-side*

koh's client is a raw byte pass-through: no key decoding, no mouse parsing, no kitty keyboard
protocol; its one interception is the `Ctrl-^` escape. That is the right shape for the phone, so the
router decodes on the server: prefix sequences, SGR 1006 mouse reports, bracketed paste. The router
sees the same bytes whether they came from the local terminal or the phone.
**Mitigation:** decode conservatively and pass anything unrecognised through to the focused pane
unchanged, so an unknown sequence degrades to tmux-like behaviour rather than being eaten.

### Two viewers, one grid — *last resize wins, for now*

A phone at 40×90 and a desktop at 200×60 cannot both see the workspace at native size.
**Mitigation:** follow the most recent resize, which koh's coalescing already implements; each pty is
resized to its pane. Add tmux's smallest-client rule when it hurts.

### Diff cost — *bounded and measurable*

`diff_from` compares each pane's grid against the base, linear in visible cells; a 200×60
workspace is 12k cells. The client's ratatui diff is the same size. koh's snapshot gating already
rate-limits how often the host is asked for a state.
**Mitigation:** keep a per-pane dirty flag from the drain so unchanged panes are skipped in both
diff and snapshot; measure with the chaos harness before optimising further.

### koh 0.11 is a real release, not a patch — *shipped 3 Sep 2026*

Generic server and client, the host trait, shared sessions, the predictor trait, OSC 9;4 capture,
the bell hook. All additive, the pty path and wire protocol (`PROTOCOL_VERSION` 3) unchanged, but
it was the largest koh change since the backend seam. It is on crates.io as 0.11.0; fux pins it
exactly. Retired as a risk.

---

## Read from source

All paths relative to `references/`.

- `koh/src/lib.rs:36`, `:41` — public API stability: the four config types, entry points, and `ssp::{SyncState, Transport}`; everything else unstable
- `koh/src/lib.rs:32` — layering law: `wire ← ssp ← {terminal, input}`; `server`/`client` never `use crate::wire`
- `koh/src/terminal/mod.rs:128`, `:259`, `:307`, `:327` — `TerminalScreen`; `ScreenDiff`; `diff_from` = `state_diff`; `apply` feeds bytes to a parser
- `koh/src/terminal/mod.rs:97`, `:203` — `blank_screen` and `from_bytes`: every screen comes from a `vt100::Parser`
- `koh/src/terminal/server.rs:33`, `:107`, `:150`, `:251` — `Callbacks` (OSC 0/1/2/52, BEL); `ServerTerminal`; `process`; `snapshot`
- `koh/src/ssp/mod.rs:82` — `SyncState` trait bounds; `ssp/transport.rs:52` — `Transport<Local, Remote>` is generic over any state
- `koh/src/ssp/mod.rs:43` — scheduler constants: 20–250 ms send interval, 32 sent states, 1024 received
- `koh/src/server/session.rs:31`, `:53`, `:59`, `:90`, `:141` — `Session { emu, pty, … }`; sessions keyed by peer `EndpointId`; `spawn_session`; `drain`; `attach`
- `koh/src/server/mod.rs:101`, `:120`, `:246`, `:293`, `:299` — resize coalescing keeps the last; `ServerSession`; `run_attached` loop; `pty.write_input`; pty + emu resize
- `koh/src/server/cli.rs:45`, `:54`, `:55`, `:74` — `ServeConfig`; `command` argv; `scrollback` (default 1000, max 1 000 000); `DEFAULT_SESSION_TTL_SECS = 86_400`
- `koh/src/pty.rs:43`, `:138`, `:156`, `:275`, `:287`, `:300` — `build_command`; `Pty::spawn(rows, cols, argv, term)`; `TERM`; `write_input`; `resize`; `try_wait`
- `koh/src/client/cli.rs:321`, `:337` — raw stdin passthrough thread; SIGWINCH resize
- `koh/src/client/mod.rs:40`, `:392` — the `Ctrl-^` escape machine, the client's only key interception
- `koh/src/client/render.rs:24`, `:158`, `:246`, `:250` — cell-by-cell repaint; `WindowState`; bell on count increase; input modes re-asserted locally (mouse passes through undecoded)
- `koh/src/client/backend/mod.rs:106` — `KohBackend` trait; termina, crossterm, qwertty impls
- `koh/src/predict.rs` — local-echo prediction, no `crate::` imports; reusable per pane
- `koh/src/wire.rs:42`, `:34`, `:96`; `transport_iroh/mod.rs:58`, `:620` — `PROTOCOL_VERSION = 3`; 1200-byte datagrams; 16 MiB decode cap; ALPN; `IrohChannel` over unreliable datagrams, no streams
- `koh/Cargo.toml` — `iroh = "=1.0.0"`, `vt100 = "=0.16.2"`, `portable-pty = "=0.9.0"`; `rust-version = "1.91"`; `unsafe_code = "forbid"`, `dead_code = "deny"`, clippy panic denies; `panic = "unwind"` required for vt100 containment
- `koh/README.md:73` — "As a library"; `:109` — no scrollback sync, no Windows
- `vt100-0.16.2/src/screen.rs:113`, `:148`, `:273`, `:534` — `set_scrollback` viewport; `rows`; `rows_formatted`; `cell`
- `herdr/src/layout.rs:1`, `:73`, `:84`, `:350` — BSP tree: `Node`, `TileLayout`, `find_in_direction`; depends only on ratatui `Rect`/`Direction`
- `herdr/src/detect/manifests/claude.toml` — rule regions, priorities, negative guards
- `herdr/src/detect/manifest.rs:1104` — `validate_region_name`, the region vocabulary
- `herdr/src/pane/agent_detection.rs:5` — idle confirmation hysteresis constants
- `herdr/src/platform/linux.rs:554`, `macos.rs:547`, `:643` — `notify-send`; `terminal-notifier` first, `osascript` fallback
- `herdr/build.rs:6`, `Cargo.toml` — libghostty-vt built with zig: why herdr's emulator is not reused; ratatui 0.30, the version fux's compositor targets
- `herdr/src` — 224k lines total; `client/` 35k, `server/` 22.5k, `pane/` 10.6k, `detect/` 5.3k

---

## Open questions

None blocking. Everything raised in the audits of 2 and 3 Sep 2026 is settled below.

### Settled

- **koh's stable API covers everything but the generic seams.** Config types with public fields,
  `Transport` already generic over the state, clap off by default. The host trait is
  `snapshot() -> S`, `input`, `resize`, `changed`, `exited`; the pty host stays as it is. Decided
  3 Sep 2026.
- **A composited `vt100::Screen` cannot be built from cells**, only from a parser; one reason
  the synced state is the workspace, not a screen.
- **Input decoding is fux's job on the host.** koh's client stays a byte pipe for keys.
- **herdr's UI and emulator are not reusable.** libghostty-vt via zig and ratatui; only the
  layout tree, manifests, constants and notifier paths port.
- **Termux clears the floor.** rust 1.98 on termux-packages `master`, koh's floor is 1.91.
- **No plugin system.** A local control socket with commands and events, panes and popups as the
  UI surface, config bindings and hooks for the rest. See *Control, not plugins*.
- **ratatui paints on the client.** `ratatui-core` for `Buffer`, `Rect` and `Layout`,
  `ratatui-widgets` for borders, tab bar, status line and popups; not the umbrella crate. A fux
  `Backend` over koh's `KohBackend` turns ratatui's buffer diff into the real terminal repaint.
  Each pane is a widget over its `PaneView` grid, marking wide-glyph continuations as skip cells.
  Panes stay `vt100::Screen` on the host; ratatui is never the emulator. herdr's `layout.rs`
  already targets ratatui's `Rect`, so it ports as a copy. Pure Rust; builds on Termux.
- **Sync the workspace, composite on the client** (tier A above). Decided 3 Sep 2026.
- **Prediction per pane on the client, in v1.** koh's predictor over the focused `PaneView` via a
  cell-reader trait. Decided 3 Sep 2026.
- **Windows is out of scope**, as in koh. Decided 3 Sep 2026.
- **Named workspaces, like tmux sessions.** One server per user hosts many workspaces; `fux
  [name]` attaches or creates, bare `fux` opens a picker (or the only workspace). Decided 3 Sep 2026.
- **Tabs in v1.** Each workspace holds a list of layout trees with a tab bar in the status line;
  `zoom` is a per-tab toggle and the phone's default view. Decided 3 Sep 2026.
- **OSC 9;4 progress via `unknown_osc` in koh's `Callbacks`.** Detection reads progress from the
  pane's terminal; `osc_progress` rules stay enabled. Decided 3 Sep 2026.
- **Bell hook `--on-bell` on `koh connect`** (`ConnectConfig.bell_command`) for plain koh users
  on Termux. fux's own client sees agent state in the replica and notifies directly. Decided
  3 Sep 2026.
- **Copy mode in v1 includes mouse selection.** Scroll viewport, keyboard selection, and
  drag-select with SGR mouse; copy goes out over koh's existing OSC 52 path. A pane that has
  mouse reporting on gets the mouse unless a modifier (default Shift) claims it for selection,
  the xterm convention. Decided 3 Sep 2026.
- **No control API versioning in v1.** Decided 3 Sep 2026: no version field and no handshake
  until something actually changes; scripts written against v1 are on notice.

---

## Your side: koh and everything else outside this repo

### koh (0.11) — *shipped as 0.11.0, 3 Sep 2026 (PR #14)*

- [x] **Generic server.** `serve_with` over `S: SyncState` and `SessionHost` (`snapshot`, `input`,
      `resize(client, ..)`, `stamp_echo_ack`, `alive`, `attach_notify`, `client_detached`);
      `PtyHost` yields `TerminalScreen` as today. State type selected by ALPN.
- [x] **Generic client.** `connect_with` over `ClientState`; `ClientTerminal<S>::render` and
      `client::InputModes`. `KohBackend` unchanged.
- [x] **Shared sessions.** `SharedHost` and `ClientId`; every peer attaches to one host and
      `resize` carries the client id so the host can choose pane geometry.
- [x] **Predictor over `predict::ScreenView`** instead of `vt100::Screen` directly.
- [x] **OSC 9;4 progress** via `ServerTerminal::progress()`; other unhandled OSCs via a bounded
      `take_unhandled_oscs()` ring instead of an `unknown_osc` callback.
- [x] **Bell hook on the client.** `--on-bell <cmd>` / `ConnectConfig::bell_command` /
      `client::BellHook`, rate-limited to one spawn per second.
- [x] **Agent-CLI rendering test** with plain koh from a phone: Claude Code renders correctly.
- [ ] Optional: **local channel** behind the `IrohChannel` seam for unix-socket attach (not in
      0.11; local attach goes over iroh loopback).

### herdr

- [ ] **Attribution.** herdr's Apache-2.0 LICENSE text into a NOTICE entry before the manifests
      or the ported `layout.rs` land.
- [ ] Optional: upstream manifest fixes so the fixture set stays shared.

### crates.io and accounts

- [ ] **`fux` is already yours** (0.1.0 placeholder, 18 Aug 2026).
- [ ] **Public remote for fux.** `references/` stays gitignored; CI uses crates.io koh and vendored
      herdr manifests. Nothing else is needed from the reference trees.
- [ ] **CI:** stable Rust at 1.91 floor, macOS and Linux; `aarch64-linux-android` cross build for
      the phone binary once fux is public. No wasm target.

### Decisions already made

- fux is one crate, one binary, one build. No Cargo features; phone and desktop builds are
  identical and each can host or attach.
- fux stays MIT; koh is MIT as of 0.10.0.
- Build the multiplexer, do not embed zellij. Depend on koh 0.11.0 as a library, generic over the
  synced state with the state type selected by ALPN.
- The synced state is the workspace; the client composites with ratatui-core and ratatui-widgets,
  no ratatui backend crate.
- All workspace logic is on the host; the client renders, predicts, and selects.
- Local attach uses the same transport as remote attach.
- Manifests port verbatim; the evaluator is rewritten; the layout tree is ported from herdr.
- Programmatic control over a unix socket instead of plugins; the CLI is a thin client for it.
- Named workspaces, tabs, zoom, and mouse selection are all v1.
