# The Shape of fux

An agent workspace: a small native terminal multiplexer built directly on koh's per-pty emulator and
mosh-grade peer-to-peer attach path, with herdr's agent-state detection running natively on the
panes. One `cargo install`-able binary, one process, no sandbox.

- **Status:** proposal, audited against source. Supersedes the zellij-based draft of 2 Sep 2026.
- **Date:** 3 Sep 2026
- **Reference trees:** `references/` — koh 0.10.0 (fa637c2) · herdr 0.8.2 (94f6d9c,
  github.com/herdrdev/herdr) · zellij `main` at 0.46.0 (af38660), kept for grid edge cases only
- **Published:** koh **0.10.0** (crates.io, 3 Sep 2026; fux depends on it, `default-features =
  false`, `backend-termina`) · fux 0.1.0 placeholder. herdr proper is not published; the `herdr`
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

## Architecture: one process, three layers, no boundary

```
  phone / other terminal            DESKTOP — ONE NATIVE PROCESS (fux server)
  ┌──────────────┐                  ┌────────────────────────────────────────────────────┐
  │ fux connect  │   iroh QUIC      │  TRANSPORT (koh, unchanged)                         │
  │ = koh client │ <──datagrams──>  │  iroh endpoint · SSP Transport<TerminalScreen,      │
  │  raw keys →  │   ScreenDiff     │  UserInput> · ServerTerminal "virtual screen"       │
  │  ← repaint   │                  │        ▲ escape bytes           │ keys, resize       │
  └──────────────┘                  │        │                        ▼                   │
  ┌──────────────┐   iroh loopback  │  WORKSPACE (fux)                                    │
  │ fux (local)  │ <──────────────> │  compositor · BSP layout · input router · status    │
  │ = same client│                  │  line · scroll mode · detection · notifier          │
  └──────────────┘                  │        ▲ vt100::Screen per pane │ bytes to pty       │
                                    │        │                        ▼                   │
                                    │  PANES (koh, unchanged)                             │
                                    │  Pty + ServerTerminal  ×N   (one per agent CLI)     │
                                    └────────────────────────────────────────────────────┘
```

| Layer | Built from | New code |
|---|---|---|
| **Panes** | koh `pty::Pty` + `terminal::ServerTerminal`, one pair per pane. Bytes in, `vt100::Screen` out, with OSC 0/1/2/52 and BEL captured by callbacks. | none |
| **Workspace** | fux. A BSP split tree ported from herdr's `layout.rs`, a ratatui-core compositor that paints pane screens plus borders and a status line into an escape-byte stream, an input router that decodes keys and SGR mouse from the client byte stream, scroll mode over `vt100::Screen::set_scrollback`, detection over each pane's screen, and the control socket. | ~5.5k lines |
| **Transport** | koh `ssp::Transport`, `transport_iroh`, session retention, allow-lists, keys. The composited screen is a `ServerTerminal` like any other; koh diffs and ships it. | none |

### The compositor writes escape bytes, because that is the only way in

koh's wire delta is `ScreenDiff.vt = vt100::Screen::state_diff(base)`, and a `vt100::Screen` exists
only as the output of a `vt100::Parser`; `TerminalScreen::from_bytes` and `blank_screen` both go
through a parser. There is no cell-level constructor. So the compositor does what tmux's
screen-write layer does: for each pane it reads `Screen::cell(row, col)` (contents, wide-char flags,
fg, bg, bold, dim, italic, underline, inverse) and emits cursor moves plus minimal SGR into one
`ServerTerminal` sized to the client's terminal. That virtual screen is the session's screen; koh's
`state_diff` against the previous frame is the wire payload, exactly as if a real program had drawn
it. The composite terminal owns the input modes: it asserts SGR mouse reporting and bracketed paste
so the router receives clicks and pastes, and forwards them to a pane only if that pane's own screen
has the mode set.

The alternative, a fux-specific multi-pane `SyncState` synced directly (the `Transport` is generic
over any `Clone + Default + PartialEq` state with a serde diff), would move compositing to the phone
and save bandwidth on partial redraws. It is not v1: it would need a fux client, and the phone should
stay a plain `koh connect`.

### One workspace, many attachments

koh today keys one detachable session per client endpoint id, with a `Pty` inside each `Session`.
fux wants the opposite: one workspace, attached to by the local terminal and any allowed phone at
once, driven by an in-process compositor instead of a pty. That is the one change koh must take (see
the koh checklist): a session host trait with `write_input`, `resize`, an output byte stream and
`try_wait`, implemented by `Pty` today and by fux's workspace tomorrow, plus a `shared` mode in which
every authorized peer attaches to the same host. Everything else in `run_attached` (snapshot gating,
input coalescing, cursor-key normalisation, reaping) applies unchanged.

Resize policy with several viewers: koh's input coalescing already keeps only the last resize per
frame, so the workspace follows the most recent resize from any client. tmux's "smallest attached
client" rule can come later.

### Local attach is remote attach over loopback

`fux` with no arguments finds or starts the server, then runs koh's client against it over iroh on
the local machine. One code path for local and phone, and detach, reconnect and prediction come for
free. It costs a QUIC handshake and one SSP loop per local viewer, which is what mosh users pay for
`tmux` inside `mosh` and is not noticeable. A unix-socket channel behind the same trait as
`IrohChannel` is a later optimisation, not a v1 requirement. The server daemonises on first `fux`,
the way tmux and zellij do; `fux serve` in the foreground is the same server for people who want it
under a supervisor.

---

## Surface: what the binary does

```sh
fux                          # attach to the local workspace, starting the server if needed
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

Inside the workspace, a tmux-style prefix (default `Ctrl-a`) then: `|` `-` split, `hjkl` focus,
`x` close, `c` new pane, `[` scroll mode, `d` detach, `?` help. Mouse click focuses a pane; wheel
scrolls the pane under the cursor. Everything else goes to the focused pty verbatim. Configuration
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
| `list` | panes with id, command, pid, cwd, title, agent, state, geometry, focus |
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
- **Phone:** the composited screen already carries koh's out-of-band title and bell. On a
  blocked transition the workspace bumps the composite bell count and sets the title to
  `fux: 2 blocked`. koh's client rings the local terminal on a bell-count increase; a small
  `--on-bell <cmd>` hook on `koh connect` (or the equivalent field on `ConnectConfig`) lets Termux
  run `termux-notification`. That is one koh change and no protocol change. A dedicated state field
  on `ScreenDiff` is the upgrade path if the title channel proves too coarse.
- **Status line:** every pane shows agent and state; blocked panes are highlighted; the tab-less
  v1 status line is one row.

---

## Distribution: one crate, two feature sets, nothing to embed

- `cargo install fux` builds the default `workspace` feature: koh's library tree plus the tiler and
  detection. No wasm, no committed artifacts, no `include_bytes!`.
- `cargo install fux --no-default-features` builds a client-only binary: `connect`, `id`, `key`,
  which is koh's client tree. This is the slim option for a phone that only ever attaches; the full
  build also compiles on Termux, so a phone can host a workspace and be attached to from the
  desktop. koh's Termux install notes (`pkg install rust clang pkg-config`) and its Android test
  suites apply directly. Termux ships rust 1.98, above koh's 1.91 floor.
- Prebuilt `aarch64-linux-android` binaries are the primary phone distribution once fux is public;
  `cargo install` is the fallback.

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

### vt100 fidelity is now load-bearing for every pane — *test it first*

Under zellij, koh's `vt100` only had to render zellij's output. Now every agent CLI renders through
it. koh already hosts shells and has parity tests ported from mosh, but Claude Code, Codex and the
rest use box drawing, wide glyphs, synchronized output, and OSC sequences vt100 may drop.
**Mitigation:** the compatibility test needs no fux code: `koh serve --allow <phone-id> --shell
claude` and drive it from a phone. Do this before writing the compositor. Where vt100 falls short,
koh owns the pin and can patch or fork it; it is a 5k-line crate.

### Input decoding does not exist in koh — *build it in the router, server-side*

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

### Compositing cost — *bounded and measurable*

Painting N panes' cells into the virtual screen on every change, then `state_diff` against the last
frame. A 200×60 grid is 12k cells; both passes are linear in cells and native. The expensive
operation in koh's loop is the snapshot clone, and it is already rate-gated.
**Mitigation:** composite only panes whose screen changed since the last frame; measure with the
chaos harness before optimising further.

### koh needs two small changes — *same author, same week*

The host trait plus shared-session mode, and the bell hook. Both are additive and land as koh 0.11.
Until they do, fux develops against a git dependency on the koh branch and pins the release when it
ships.

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

In the order they block work.

1. **Does vt100 render the agent CLIs?** Testable today with plain koh from a phone:
   `koh serve --allow <phone-id> --shell claude`, then Codex, then a full-screen app. Exercise
   resize, spinner, prompt box, scrollback. This is the one risk that can sink the design.
2. **Session host trait shape in koh.** Minimum: `write_input(&[u8])`, `resize(rows, cols)`,
   an output `Receiver<Vec<u8>>`, `try_wait() -> Option<ExitStatus>`. Does the drain task stay in
   koh (host yields bytes) or move to the host (host feeds a `ServerTerminal`)? Bytes-out is
   simpler and keeps `Session` unchanged.
3. **Bare `fux` semantics.** One workspace per user, named `main`, in `$XDG_RUNTIME_DIR`, or
   named workspaces like tmux sessions? Recommendation: one workspace per user for v1; tabs later
   cover the "several projects" case without a second server.
4. **Tabs in v1?** herdr has workspaces, zellij has tabs. A BSP tree with 4 to 6 agent panes
   fits a laptop screen; on the phone it does not, and the phone wants one pane at a time.
   Recommendation: no tabs, but a **zoom** toggle (focused pane fills the screen), which is also the
   phone's default view.
5. **OSC 9;4 progress.** Add `unknown_osc` to koh's `Callbacks` or parse it in fux's drain?
6. **Phone notification hook.** `--on-bell` on `koh connect` versus a `bell_command` field in
   `ConnectConfig` used by fux's own client build. The first serves plain koh users too.
7. **Copy mode.** Scroll mode ships in v1; selection and copy via OSC 52 (koh already forwards
   clipboard out-of-band) can be v1.1.
8. **Prediction per pane.** koh's predictor runs on the client over the composite. Typing into a
   pane still predicts correctly for plain text; it will mispredict across borders. Acceptable for
   v1; measure.
9. **Windows.** Out of scope, as in koh.

### Settled

- **koh's stable API covers everything but the host seam.** Config types with public fields,
  `Transport` generic over the state, clap off by default.
- **The compositor must produce escape bytes.** No cell-level screen constructor exists in
  vt100 0.16.2 or koh.
- **Input decoding is fux's job on the server.** koh's client is deliberately a byte pipe.
- **herdr's UI and emulator are not reusable.** libghostty-vt via zig and ratatui; only the
  layout tree, manifests, constants and notifier paths port.
- **Termux clears the floor.** rust 1.98 on termux-packages `master`, koh's floor is 1.91.
- **No plugin system.** A local control socket with commands and events, panes and popups as the
  UI surface, config bindings and hooks for the rest. See *Control, not plugins*.
- **ratatui paints the composite.** `ratatui-core` for `Buffer`,
  `Rect` and `Layout`, `ratatui-widgets` for borders, the status line and popups; not the umbrella
  crate, so no second terminal backend enters the server. fux implements ratatui's `Backend` for
  koh's virtual `ServerTerminal`: `draw` turns changed cells into cursor moves and SGR bytes fed
  to `process`. Each pane is a widget that copies `vt100::Screen::cell` into `Buffer` cells,
  marking wide-glyph continuations as skip cells. Panes stay `vt100::Screen`; ratatui is never
  the emulator. herdr's `layout.rs` already targets ratatui's `Rect`, so it ports as a copy. It
  lives with the tiler under the `workspace` feature only because a client-only build has nothing
  to paint; it is pure Rust and builds on Termux, so a phone can host a workspace too.
- **No control API versioning in v1.** Decided 3 Sep 2026: no version field and no handshake
  until something actually changes; scripts written against v1 are on notice.

---

## Your side: koh and everything else outside this repo

### koh (0.11)

- [ ] **Session host trait and shared-session mode.** Abstract `Pty` behind a trait in
      `server::session`; add `ServeConfig.host` (or a `serve_with(config, host_factory)`) so an
      embedding binary can supply the in-process workspace; add a mode where all authorized peers
      attach to one host instead of one session per endpoint id. Keeps `koh` binary behaviour.
- [ ] **Bell hook on the client.** `--on-bell <cmd>` / `ConnectConfig.bell_command`, run on a
      bell-count increase. Termux users get `termux-notification` for free.
- [ ] **`unknown_osc` in `Callbacks`** (or expose raw drain bytes) for OSC 9;4.
- [ ] **Run the agent-CLI rendering test** (open question 1) with plain koh from a phone.
- [ ] Optional: **local channel** behind the `IrohChannel` seam for unix-socket attach.

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

- fux is one crate, one binary. Default `workspace` feature is the server and tiler;
  `--no-default-features` is the client-only slim build. Both build on Termux.
- fux stays MIT; koh is MIT as of 0.10.0.
- Build the multiplexer, do not embed zellij. Depend on koh as a library; the only koh change is
  the session host seam plus two small hooks.
- The phone runs an unmodified koh client. All workspace logic is server-side.
- Local attach uses the same transport as remote attach.
- Manifests port verbatim; the evaluator is rewritten; the layout tree is ported from herdr.
- ratatui-core and ratatui-widgets render the composite; no ratatui backend crate.
- Programmatic control over a unix socket instead of plugins; the CLI is a thin client for it.
